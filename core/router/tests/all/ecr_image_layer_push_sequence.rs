// SPDX-License-Identifier: CC0-1.0

//! Pushing an image LAYER into a private Amazon ECR repository on the assembled
//! emulator, through the sequence a registry client uses.
//!
//! The RULE under test: **a layer enters a repository through
//! `InitiateLayerUpload` -> `UploadLayerPart` -> `CompleteLayerUpload`, and what the
//! registry ends up holding is the CONCATENATION of the parts it accepted, whatever part
//! boundaries the client chose — each part whose declared `[partFirstByte, partLastByte]`
//! range spans exactly as many bytes as its `layerPartBlob` carries and begins where the
//! upload's received bytes end is appended and answered with the last byte the registry
//! now holds, and the completed upload registers those assembled bytes under the
//! `sha256:<hex>` digest of the content itself. The registry then reports that layer
//! `AVAILABLE` with the number of bytes pushed, serves exactly those bytes at the
//! download URL it announces for it, and accepts a manifest that references it, which it
//! serves back under the tag the push carried.**
//!
//! So the part boundaries are a client's transport decision and nothing else: cutting one
//! layer up differently yields the same digest, the same byte count and the same served
//! bytes.
//!
//! The suite drives the sequence on the ASSEMBLED emulator — every service mounted, as
//! `build_router` assembles the one the binary boots — so it also pins that each
//! operation of the sequence is claimed by Amazon ECR among every other service's claims,
//! and that the announced download URL is served by that assembled emulator to a client
//! carrying no credential of its own. A crate-local suite sees neither.

use std::collections::BTreeMap;
use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every call of this suite is signed for, and whose registry therefore holds
/// the repositories it creates.
const REGION: &str = "us-east-1";

/// Amazon ECR's `X-Amz-Target` prefix, from that service's own model.
const ECR_TARGET_PREFIX: &str = "AmazonEC2ContainerRegistry_V20150921";

/// The media type a Docker image manifest V2 schema 2 declares, which is what the pushed
/// manifests below are.
const MANIFEST_MEDIA_TYPE: &str = "application/vnd.docker.distribution.manifest.v2+json";

/// The media type of a gzipped layer blob, as a schema 2 descriptor spells it.
const LAYER_MEDIA_TYPE: &str = "application/vnd.docker.image.rootfs.diff.tar.gzip";

/// The media type of the image config blob a schema 2 manifest references.
const CONFIG_MEDIA_TYPE: &str = "application/vnd.docker.container.image.v1+json";

/// The emulator this suite drives: the whole router over one engine.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = aws_emulator_router::build_router(engine);
        // These rows exercise the push sequence, not request signing.
        router.set_request_authentication(false);
        Self { router }
    }

    /// One Amazon ECR call on its awsJson1_1 wire, with the status it answered.
    async fn ecr(&self, operation: &str, body: &Value) -> (u16, Value) {
        let host = format!("ecr.{REGION}.amazonaws.com");
        let request = http::Request::builder()
            .method("POST")
            .uri(format!("https://{host}/"))
            .header("host", host)
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/ecr/aws4_request, \
                     SignedHeaders=host, Signature=0000"
                ),
            )
            .header("content-type", "application/x-amz-json-1.1")
            .header("x-amz-target", format!("{ECR_TARGET_PREFIX}.{operation}"))
            .body(Bytes::from(body.to_string()))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let payload = response.into_body();
        let parsed = if payload.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&payload).unwrap_or_else(|_| {
                json!({ "unparsed": String::from_utf8_lossy(&payload).into_owned() })
            })
        };
        (status, parsed)
    }

    /// One Amazon ECR call that has to succeed.
    async fn ecr_ok(&self, case: &str, operation: &str, body: &Value) -> Value {
        let (status, answer) = self.ecr(operation, body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: ecr {operation} must answer 200, got {status} with {answer}"
        );
        answer
    }

    /// One Amazon ECR call that has to be refused, answering the error code it named.
    async fn ecr_refused(&self, case: &str, operation: &str, body: &Value) -> String {
        let (status, answer) = self.ecr(operation, body).await;
        assert_eq!(
            status, 400,
            "case {case:?}: ecr {operation} must be refused with HTTP 400, got {status} with \
             {answer}"
        );
        error_code(&answer)
    }

    /// Follow an announced URL exactly as a client follows one: the method and URI it was
    /// given, addressed to the authority the URL itself names, carrying NO credential.
    async fn fetch(&self, url: &str) -> (u16, Bytes) {
        let uri: http::Uri = url.parse().expect("the announced URL is a URI");
        let authority = uri
            .authority()
            .map(|authority| authority.as_str().to_string())
            .unwrap_or_default();
        let request = http::Request::builder()
            .method("GET")
            .uri(url)
            .header("host", authority)
            .body(Bytes::new())
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        (status, response.into_body())
    }
}

/// The error code an awsJson1_1 refusal names, read the way a client reads it: the shape
/// name at the end of `__type`.
fn error_code(body: &Value) -> String {
    let named = body["__type"]
        .as_str()
        .or_else(|| body["code"].as_str())
        .unwrap_or_default();
    named
        .rsplit_once(['#', ':'])
        .map_or(named, |(_prefix, code)| code)
        .to_string()
}

/// The content digest of `content`: the `sha256:<hex>` form a registry addresses a blob
/// by, computed here independently of the emulator's own helper.
fn content_digest(content: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("sha256:{}", hex::encode(Sha256::digest(content)))
}

/// `len` layer bytes that depend only on `len`: a deterministic spread over the whole
/// byte range, so a blob carries `0x00` and `0xff` like real compressed layer content and
/// two rows asking for the same length push the same bytes — which is what lets the rows
/// below compare the digests of one layer cut up in different ways.
fn layer_bytes(len: usize) -> Vec<u8> {
    (0..len)
        .map(|index| u8::try_from((index * 31 + 13) % 256).unwrap_or_default())
        .collect()
}

/// A `layerPartBlob` member: a blob travels base64-encoded on the awsJson1_1 wire, with
/// the codec the wire serializer itself uses.
fn blob(bytes: &[u8]) -> Value {
    Value::String(aws_smithy_types::base64::encode(bytes))
}

/// Open an upload against `repository` and answer the upload id it handed out.
async fn open_upload(emulator: &Emulator, case: &str, repository: &str) -> String {
    let opened = emulator
        .ecr_ok(case, "InitiateLayerUpload", &json!({"repositoryName": repository}))
        .await;
    opened["uploadId"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: InitiateLayerUpload must hand back an uploadId, got {opened}")
        })
        .to_string()
}

/// One `UploadLayerPart` request body.
fn part_request(
    repository: &str,
    upload_id: &str,
    part_first_byte: i64,
    part_last_byte: i64,
    bytes: &[u8],
) -> Value {
    json!({
        "repositoryName": repository,
        "uploadId": upload_id,
        "partFirstByte": part_first_byte,
        "partLastByte": part_last_byte,
        "layerPartBlob": blob(bytes),
    })
}

/// Stream the bytes of `content` from `from` onward into the upload `upload_id`, as the
/// parts `cuts` describes, asserting each answer as it goes — `from` is what the upload
/// already holds, so a resumed upload continues at the byte it ended on.
async fn stream_parts(
    emulator: &Emulator,
    case: &str,
    repository: &str,
    upload_id: &str,
    content: &[u8],
    from: usize,
    cuts: &[usize],
) {
    let mut offset: usize = from;
    for (index, size) in cuts.iter().copied().enumerate() {
        let part = &content[offset..offset + size];
        let first = i64::try_from(offset).expect("a test offset fits in an i64");
        let last = first + i64::try_from(size).expect("a test part fits in an i64") - 1;
        let answer = emulator
            .ecr_ok(
                case,
                "UploadLayerPart",
                &part_request(repository, upload_id, first, last, part),
            )
            .await;
        assert_eq!(
            answer["lastByteReceived"].as_i64(),
            Some(last),
            "case {case:?} part {index}: lastByteReceived must be the last byte of the accepted \
             part ({last}), got {answer}"
        );
        assert_eq!(
            answer["uploadId"].as_str(),
            Some(upload_id),
            "case {case:?} part {index}: the answer must name the upload addressed, got {answer}"
        );
        assert_eq!(
            answer["repositoryName"].as_str(),
            Some(repository),
            "case {case:?} part {index}: the answer must name the upload's repository, got \
             {answer}"
        );
        assert_eq!(
            answer["registryId"].as_str(),
            Some(engine::emulator_account_id()),
            "case {case:?} part {index}: the answer must name this account's registry, got \
             {answer}"
        );
        offset += size;
    }
    assert_eq!(
        offset,
        content.len(),
        "case {case:?}: the cuts {cuts:?} must describe the layer up to its last byte, got \
         {offset} bytes"
    );
}

/// Close the upload `upload_id` by asserting `content`'s digest, and answer the digest the
/// registry registered the layer under.
async fn complete_upload(
    emulator: &Emulator,
    case: &str,
    repository: &str,
    upload_id: &str,
    content: &[u8],
) -> String {
    let digest = content_digest(content);
    let completed = emulator
        .ecr_ok(
            case,
            "CompleteLayerUpload",
            &json!({
                "repositoryName": repository,
                "uploadId": upload_id,
                "layerDigests": [&digest],
            }),
        )
        .await;
    assert_eq!(
        completed["layerDigest"].as_str(),
        Some(digest.as_str()),
        "case {case:?}: the completed upload must register the layer under the digest of its own \
         bytes ({digest}), got {completed}"
    );
    assert_eq!(
        completed["uploadId"].as_str(),
        Some(upload_id),
        "case {case:?}: the answer must name the upload closed, got {completed}"
    );
    completed["layerDigest"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// Push `content` into `repository` as one layer, cut up as `cuts` says, and answer the
/// digest it was registered under.
async fn push_layer(
    emulator: &Emulator,
    case: &str,
    repository: &str,
    content: &[u8],
    cuts: &[usize],
) -> String {
    let upload_id = open_upload(emulator, case, repository).await;
    stream_parts(emulator, case, repository, &upload_id, content, 0, cuts).await;
    complete_upload(emulator, case, repository, &upload_id, content).await
}

/// The `Layer` the registry reports for `digest`, as `BatchCheckLayerAvailability` answers
/// it.
async fn reported_layer(emulator: &Emulator, case: &str, repository: &str, digest: &str) -> Value {
    let answer = emulator
        .ecr_ok(
            case,
            "BatchCheckLayerAvailability",
            &json!({ "repositoryName": repository, "layerDigests": [digest] }),
        )
        .await;
    answer["layers"]
        .as_array()
        .and_then(|layers| layers.first().cloned())
        .unwrap_or_else(|| {
            panic!("case {case:?}: the registry must report the layer {digest}, got {answer}")
        })
}

/// A Docker image manifest V2 schema 2 referencing the config blob and the layer blob the
/// row pushed, with the sizes the descriptors state.
fn manifest(
    config_digest: &str,
    config_size: usize,
    layer_digest: &str,
    layer_size: usize,
) -> String {
    json!({
        "schemaVersion": 2,
        "mediaType": MANIFEST_MEDIA_TYPE,
        "config": {
            "mediaType": CONFIG_MEDIA_TYPE,
            "digest": config_digest,
            "size": config_size,
        },
        "layers": [{
            "mediaType": LAYER_MEDIA_TYPE,
            "digest": layer_digest,
            "size": layer_size,
        }],
    })
    .to_string()
}

/// One row: how a client cuts ONE layer up, and the layer it is cutting.
struct Row {
    case: &'static str,
    /// The repository the row pushes into, inside the modelled `RepositoryName` pattern.
    repository: &'static str,
    /// The sizes of the parts, in the order they are streamed. Their sum is the layer's
    /// length, and the content is derived from that length alone, so two rows summing to
    /// the same length push the same bytes.
    cuts: &'static [usize],
}

/// The rows differ along what the rule generalises over: how many parts the layer is cut
/// into and how big each one is — one part carrying everything, equal parts, parts of
/// unequal sizes, single-byte parts, a layer that is one byte, and a part of tens of
/// kilobytes followed by a small one. Three of them cut the SAME 4096-byte layer, which is
/// what makes "the boundaries are not part of the content" observable.
const ROWS: &[Row] = &[
    Row {
        case: "one part carrying the whole layer",
        repository: "push-one-part",
        cuts: &[4096],
    },
    Row {
        case: "four equal parts",
        repository: "push-equal-parts",
        cuts: &[1024, 1024, 1024, 1024],
    },
    Row {
        case: "parts of unequal sizes",
        repository: "push-uneven-parts",
        cuts: &[1, 7, 4080, 8],
    },
    Row {
        case: "a layer of a single byte",
        repository: "push-single-byte",
        cuts: &[1],
    },
    Row {
        case: "many single-byte parts",
        repository: "push-byte-at-a-time",
        cuts: &[1; 16],
    },
    Row {
        case: "a part of tens of kilobytes followed by a small one",
        repository: "push-large-part",
        cuts: &[65_536, 17],
    },
];

#[tokio::test]
async fn a_layer_pushed_in_parts_is_the_concatenation_of_the_parts_whatever_the_cuts() {
    let emulator = Emulator::new();
    let registry_id = engine::emulator_account_id();
    // The digest each layer LENGTH was registered under, so the rows that cut the same
    // layer differently are checked against each other.
    let mut digest_of_length: BTreeMap<usize, String> = BTreeMap::new();

    for row in ROWS {
        let case = row.case;
        let content = layer_bytes(row.cuts.iter().sum());
        emulator
            .ecr_ok(
                case,
                "CreateRepository",
                &json!({ "repositoryName": row.repository }),
            )
            .await;

        // THE PUSH: the parts this row cuts the layer into, then the completion that
        // registers the assembled bytes.
        let layer_digest = push_layer(&emulator, case, row.repository, &content, row.cuts).await;
        assert_eq!(
            layer_digest,
            content_digest(&content),
            "case {case:?}: the layer must be registered under the digest of the bytes pushed, \
             got {layer_digest}"
        );
        if let Some(already) = digest_of_length.get(&content.len()) {
            assert_eq!(
                &layer_digest, already,
                "case {case:?}: the same {} bytes cut up differently must register the same \
                 digest, got {layer_digest} beside {already}",
                content.len()
            );
        } else {
            digest_of_length.insert(content.len(), layer_digest.clone());
        }

        // WHAT THE REGISTRY HOLDS: the layer, available, with the byte count pushed.
        let layer = reported_layer(&emulator, case, row.repository, &layer_digest).await;
        assert_eq!(
            layer["layerAvailability"].as_str(),
            Some("AVAILABLE"),
            "case {case:?}: the pushed layer must be reported available, got {layer}"
        );
        assert_eq!(
            layer["layerSize"].as_u64(),
            u64::try_from(content.len()).ok(),
            "case {case:?}: the layer must be reported as the {} bytes pushed, got {layer}",
            content.len()
        );
        assert_eq!(
            layer["layerDigest"].as_str(),
            Some(layer_digest.as_str()),
            "case {case:?}: the reported layer must be the one addressed, got {layer}"
        );

        // WHAT A PULL SERVES: the announced download URL, followed with no credential,
        // serves the bytes the parts carried, byte for byte.
        let announced = emulator
            .ecr_ok(
                case,
                "GetDownloadUrlForLayer",
                &json!({ "repositoryName": row.repository, "layerDigest": &layer_digest }),
            )
            .await;
        let url = announced["downloadUrl"].as_str().unwrap_or_else(|| {
            panic!("case {case:?}: the registry must announce a download URL, got {announced}")
        });
        let (status, served) = emulator.fetch(url).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the announced download URL must be served, got {status} with {:?}",
            String::from_utf8_lossy(&served)
        );
        assert_eq!(
            served.as_ref(),
            content.as_slice(),
            "case {case:?}: the download must serve the {} bytes the parts carried, got {} bytes",
            content.len(),
            served.len()
        );

        // AND THE IMAGE OVER IT: a manifest referencing the pushed layer is accepted and
        // served back under the tag it was pushed with.
        let config = json!({
            "architecture": "amd64",
            "rootfs": { "type": "layers" },
            "case": case,
        })
        .to_string();
        let config_digest = push_layer(
            &emulator,
            case,
            row.repository,
            config.as_bytes(),
            &[config.len()],
        )
        .await;
        let document = manifest(&config_digest, config.len(), &layer_digest, content.len());
        let pushed = emulator
            .ecr_ok(
                case,
                "PutImage",
                &json!({
                    "repositoryName": row.repository,
                    "imageManifest": &document,
                    "imageManifestMediaType": MANIFEST_MEDIA_TYPE,
                    "imageTag": "release",
                }),
            )
            .await;
        assert_eq!(
            pushed["image"]["imageId"]["imageDigest"].as_str(),
            Some(content_digest(document.as_bytes()).as_str()),
            "case {case:?}: the manifest must be stored under the digest of its own bytes, got \
             {pushed}"
        );
        let pulled = emulator
            .ecr_ok(
                case,
                "BatchGetImage",
                &json!({
                    "repositoryName": row.repository,
                    "imageIds": [{ "imageTag": "release" }],
                }),
            )
            .await;
        let image = pulled["images"]
            .as_array()
            .and_then(|images| images.first().cloned())
            .unwrap_or_else(|| {
                panic!("case {case:?}: the tagged image must be pullable, got {pulled}")
            });
        assert_eq!(
            image["imageManifest"].as_str(),
            Some(document.as_str()),
            "case {case:?}: the pull must serve the manifest that was pushed, got {image}"
        );
        assert_eq!(
            image["registryId"].as_str(),
            Some(registry_id),
            "case {case:?}: the image must be reported in this account's registry, got {image}"
        );
        assert_eq!(
            image["repositoryName"].as_str(),
            Some(row.repository),
            "case {case:?}: the image must be reported in the repository pushed to, got {image}"
        );
    }

    assert_eq!(
        digest_of_length.len(),
        4,
        "the rows must cover four distinct layer lengths, got {:?}",
        digest_of_length.keys().collect::<Vec<&usize>>()
    );
}

/// One row of the refusal table: a part the registry must not keep, and the error it is
/// refused with.
struct Refusal {
    case: &'static str,
    /// How far past (or before) the upload's received bytes the part declares itself.
    part_first_byte: i64,
    part_last_byte: i64,
    /// How many bytes the blob actually carries.
    blob_len: usize,
    code: &'static str,
}

/// A part the sequence does not admit keeps nothing and ends nothing: the upload stays
/// where the last accepted part left it, so the client's next consecutive part is accepted
/// and the layer completes with exactly the bytes it streamed.
///
/// The rows differ along how a part fails to continue the upload — a gap ahead of it, a
/// replay of bytes already received, an overlap, a range longer than its blob, a range
/// shorter than it, a range that ends before it begins — plus the two ways a part can
/// address nothing at all: an upload id this registry never issued and a repository that
/// does not exist.
#[tokio::test]
async fn a_refused_part_keeps_nothing_and_the_sequence_still_completes() {
    let emulator = Emulator::new();
    let case = "a refused part";
    let repository = "push-after-refusal";
    emulator
        .ecr_ok(
            case,
            "CreateRepository",
            &json!({ "repositoryName": repository }),
        )
        .await;

    let content = layer_bytes(64);
    let upload_id = open_upload(&emulator, case, repository).await;
    // One accepted part, so the upload holds bytes 0..=9 and the next part must begin at
    // byte 10.
    stream_parts(
        &emulator,
        case,
        repository,
        &upload_id,
        &content[..10],
        0,
        &[10],
    )
    .await;

    let rows: &[Refusal] = &[
        Refusal {
            case: "a gap of one byte ahead of the upload",
            part_first_byte: 11,
            part_last_byte: 12,
            blob_len: 2,
            code: "InvalidLayerPartException",
        },
        Refusal {
            case: "a replay of the bytes already received",
            part_first_byte: 0,
            part_last_byte: 9,
            blob_len: 10,
            code: "InvalidLayerPartException",
        },
        Refusal {
            case: "a one-byte overlap with the bytes already received",
            part_first_byte: 9,
            part_last_byte: 20,
            blob_len: 12,
            code: "InvalidLayerPartException",
        },
        Refusal {
            case: "a range longer than the blob it carries",
            part_first_byte: 10,
            part_last_byte: 29,
            blob_len: 10,
            code: "InvalidLayerPartException",
        },
        Refusal {
            case: "a range shorter than the blob it carries",
            part_first_byte: 10,
            part_last_byte: 13,
            blob_len: 10,
            code: "InvalidLayerPartException",
        },
        Refusal {
            case: "a range that ends before it begins",
            part_first_byte: 14,
            part_last_byte: 13,
            blob_len: 0,
            code: "InvalidLayerPartException",
        },
    ];

    for row in rows {
        let code = emulator
            .ecr_refused(
                row.case,
                "UploadLayerPart",
                &part_request(
                    repository,
                    &upload_id,
                    row.part_first_byte,
                    row.part_last_byte,
                    &layer_bytes(row.blob_len),
                ),
            )
            .await;
        assert_eq!(
            code, row.code,
            "case {:?}: expected {}, got {code}",
            row.case, row.code
        );
    }

    // The two ways a part addresses nothing at all: an upload id this registry never
    // issued (well-formed under the modelled 8-4-4-4-12 hex pattern) and a repository that
    // was never created.
    let never_issued = "00000000-0000-0000-0000-000000000000";
    let code = emulator
        .ecr_refused(
            "an upload id this registry never issued",
            "UploadLayerPart",
            &part_request(repository, never_issued, 0, 9, &layer_bytes(10)),
        )
        .await;
    assert_eq!(
        code, "UploadNotFoundException",
        "case {:?}: expected UploadNotFoundException, got {code}",
        "an upload id this registry never issued"
    );
    let code = emulator
        .ecr_refused(
            "a repository that was never created",
            "UploadLayerPart",
            &part_request("push-absent-repo", never_issued, 0, 9, &layer_bytes(10)),
        )
        .await;
    assert_eq!(
        code, "RepositoryNotFoundException",
        "case {:?}: expected RepositoryNotFoundException, got {code}",
        "a repository that was never created"
    );

    // NOTHING THE REFUSALS TOUCHED: the upload continues from byte 10, and the layer the
    // client assembled is exactly the bytes it streamed.
    stream_parts(
        &emulator,
        case,
        repository,
        &upload_id,
        &content,
        // The ten bytes the accepted part left in the upload...
        10,
        // ...and the rest of the layer, in two further parts.
        &[24, 30],
    )
    .await;
    let digest = complete_upload(&emulator, case, repository, &upload_id, &content).await;
    let layer = reported_layer(&emulator, case, repository, &digest).await;
    assert_eq!(
        layer["layerSize"].as_u64(),
        u64::try_from(content.len()).ok(),
        "case {case:?}: the completed layer must hold the {} bytes streamed, got {layer}",
        content.len()
    );
    let announced = emulator
        .ecr_ok(
            case,
            "GetDownloadUrlForLayer",
            &json!({ "repositoryName": repository, "layerDigest": &digest }),
        )
        .await;
    let url = announced["downloadUrl"].as_str().unwrap_or_else(|| {
        panic!("case {case:?}: the registry must announce a download URL, got {announced}")
    });
    let (status, served) = emulator.fetch(url).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the announced download URL must be served, got {status}"
    );
    assert_eq!(
        served.as_ref(),
        content.as_slice(),
        "case {case:?}: the download must serve the bytes the accepted parts carried, got {} \
         bytes",
        served.len()
    );
}
