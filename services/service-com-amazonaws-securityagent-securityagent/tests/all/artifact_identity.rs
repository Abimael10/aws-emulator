// SPDX-License-Identifier: CC0-1.0

//! The IDENTITY an artifact is handed and the sentence the artifact sub-API states when
//! it cannot resolve the agent space it was pointed at, end to end over the wire.
//!
//! Every call goes through the real `MultiServiceRouter` the emulator boots (see
//! `crate::support`), as a `restJson1` `POST /<OperationName>`; every minted id is read
//! back through `GetArtifact` before it counts, so nothing here passes against a handler
//! that answers a canned body.
//!
//! The rules this suite owns:
//!
//! * an `artifactId` is minted PER stored artifact, in the artifact class's own id form
//!   (the class prefix, then a body freshly generated for that one artifact), so no two
//!   artifacts of an account — in one agent space or in different ones — are handed one
//!   id, no artifact id is also the id of another resource class the account holds, and
//!   the artifact reads back under exactly the id the upload answered;
//! * an operation OF a space's artifacts that cannot resolve the `agentSpaceId` it was
//!   pointed at is refused with `ResourceNotFoundException` (HTTP 404) whose message is
//!   the artifact path's fixed text with that `agentSpaceId` interpolated — so a caller
//!   uploading or reading in a batch can tell WHICH space was rejected.
//!
//! Each case asserts its rule over a TABLE: the id rows differ along the agent space the
//! upload goes to, the file name, the declared type and the bytes (including the same
//! file uploaded twice byte for byte and no bytes at all), and the refusal rows along the
//! operation called and the FORM of the id that resolves to nothing.
//!
//! `crate::minted_identifier_forms` owns the id-form rule for the classes a create of
//! their own mints; `crate::artifact_storage`, `crate::artifact_deletion`,
//! `crate::artifact_listing` and `crate::artifact_metadata_batch_read` own what each
//! artifact operation otherwise does.

use std::collections::BTreeSet;

use serde_json::{json, Value};
use service_com_amazonaws_securityagent_securityagent::state::{
    AGENT_SPACE_ID_PREFIX, ARTIFACT_ID_PREFIX, SECURITY_REQUIREMENT_PACK_ID_PREFIX,
    TARGET_DOMAIN_ID_PREFIX, VERIFICATION_METHOD_DNS_TXT,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::support::{assert_uuid_body, call, emulator, member, ok, Answer};

/// The sentence an artifact operation's refusal of an agent space the account does not
/// hold states.
///
/// Source: the recorded 404 of an artifact operation whose `agentSpaceId` names no agent
/// space. Its fixed text is written out here independently of the helper the handlers
/// build it with, so a change to that helper is a failure rather than a silently
/// agreed-on new wording; the `agentSpaceId` taken from the request is its ONLY variable
/// part.
fn agent_space_not_found(agent_space_id: &str) -> String {
    format!("AgentSpace under the provided id: {agent_space_id} does not exist")
}

/// One upload: the agent space it goes to and what it carries.
struct Upload {
    case: &'static str,
    /// Which of the agent spaces the case created the upload goes to.
    space: usize,
    file_name: &'static str,
    /// An `ArtifactType` the model admits
    /// (`smithy_query(query="shape",shape="ArtifactType",
    /// model_path="services/service-com-amazonaws-securityagent-securityagent/smithy-model/com-amazonaws-securityagent-securityagent.json")`:
    /// DOC | DOCX | JPEG | JSON | MD | PDF | PNG | TXT | YAML).
    artifact_type: &'static str,
    content: &'static [u8],
}

/// `CreateAgentSpace`, as the minted `agentSpaceId`.
async fn create_agent_space(router: &MultiServiceRouter, name: &str) -> String {
    let created = ok(router, "CreateAgentSpace", &json!({"name": name}), name).await;
    member(&created, "agentSpaceId")
        .unwrap_or_else(|| panic!("case {name:?}: CreateAgentSpace minted no agentSpaceId"))
}

/// `AddArtifact`, as the whole answer (the operation's modelled success is 201).
async fn add(router: &MultiServiceRouter, agent_space_id: &str, upload: &Upload) -> Answer {
    call(
        router,
        "AddArtifact",
        &json!({
            "agentSpaceId": agent_space_id,
            "fileName": upload.file_name,
            "artifactType": upload.artifact_type,
            // A blob travels as base64 on this protocol.
            "artifactContent": aws_smithy_types::base64::encode(upload.content),
        }),
    )
    .await
}

/// `GetArtifact`, as the whole answer.
async fn get(router: &MultiServiceRouter, agent_space_id: &str, artifact_id: &str) -> Answer {
    call(
        router,
        "GetArtifact",
        &json!({"agentSpaceId": agent_space_id, "artifactId": artifact_id}),
    )
    .await
}

/// The uploads of the identity case: two spaces, the same file uploaded twice byte for
/// byte, a second space holding a file of the same name, several declared types and an
/// upload carrying no bytes at all — none of which the identity may depend on.
fn uploads() -> Vec<Upload> {
    vec![
        Upload {
            case: "a text file",
            space: 0,
            file_name: "notes.txt",
            artifact_type: "TXT",
            content: b"the service talks to one database",
        },
        Upload {
            case: "the same file and bytes again",
            space: 0,
            file_name: "notes.txt",
            artifact_type: "TXT",
            content: b"the service talks to one database",
        },
        Upload {
            case: "a diagram",
            space: 0,
            file_name: "architecture.png",
            artifact_type: "PNG",
            content: &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a],
        },
        Upload {
            case: "an empty file",
            space: 0,
            file_name: "empty.json",
            artifact_type: "JSON",
            content: b"",
        },
        Upload {
            case: "the same file name in another agent space",
            space: 1,
            file_name: "notes.txt",
            artifact_type: "TXT",
            content: b"the service talks to one database",
        },
        Upload {
            case: "a specification in another agent space",
            space: 1,
            file_name: "api.yaml",
            artifact_type: "YAML",
            content: b"openapi: 3.0.0\n",
        },
    ]
}

/// RULE: an `artifactId` is minted per stored artifact, in the artifact class's own id
/// form, so no two artifacts of an account share one id whatever they carry and whichever
/// space holds them, and each artifact reads back under the id its upload answered.
#[tokio::test]
async fn every_upload_is_handed_its_own_artifact_id_in_the_artifact_class_form() {
    let router = emulator();
    let spaces = [
        create_agent_space(&router, "artifact-identity-one").await,
        create_agent_space(&router, "artifact-identity-two").await,
    ];

    let mut minted: BTreeSet<String> = BTreeSet::new();
    let uploads = uploads();
    for upload in &uploads {
        let case = upload.case;
        let space = &spaces[upload.space];
        let answer = add(&router, space, upload).await;
        assert_eq!(
            answer.status, 201,
            "case {case:?}: expected AddArtifact to answer 201, got {} ({}) with {}",
            answer.status, answer.error_code, answer.body
        );
        let id = member(&answer.body, "artifactId")
            .unwrap_or_else(|| panic!("case {case:?}: AddArtifact minted no artifactId"));

        // The class's own form: the artifact prefix, then the hyphenated uuid body
        // generated for this one artifact.
        let body = id.strip_prefix(ARTIFACT_ID_PREFIX).unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected the artifactId to start {ARTIFACT_ID_PREFIX:?}, \
                 got {id:?}"
            )
        });
        assert_uuid_body(body, case, "artifactId");

        // A fresh id per stored artifact, whatever the upload carried and whichever space
        // holds it.
        assert!(
            minted.insert(id.clone()),
            "case {case:?}: the artifactId {id:?} was already handed to another artifact; \
             the ids so far are {minted:?}"
        );

        // The id is the STORE's: the artifact resolves under it, in the space it was
        // uploaded to and nowhere else.
        let read = get(&router, space, &id).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: expected the artifact {id:?} to read back, got {} ({}) with {}",
            read.status, read.error_code, read.body
        );
        assert_eq!(
            member(&read.body, "artifactId").as_deref(),
            Some(id.as_str()),
            "case {case:?}: expected the read to name {id:?}, got {}",
            read.body
        );
        assert_eq!(
            member(&read.body, "fileName").as_deref(),
            Some(upload.file_name),
            "case {case:?}: expected the read to report the uploaded file name, got {}",
            read.body
        );
        let other = &spaces[1 - upload.space];
        let elsewhere = get(&router, other, &id).await;
        assert_eq!(
            (elsewhere.status, elsewhere.error_code.as_str()),
            (404, "ResourceNotFoundException"),
            "case {case:?}: expected {id:?} to resolve only through the space that holds \
             it, got {} ({}) with {}",
            elsewhere.status,
            elsewhere.error_code,
            elsewhere.body
        );
    }

    assert_eq!(
        minted.len(),
        uploads.len(),
        "every upload of the table must have been handed an id of its own, got {minted:?}"
    );

    // No artifact id is also the id of another resource class this account holds: an id
    // carries its OWN class's prefix and no other's.
    let pack = call(
        &router,
        "CreateSecurityRequirementPack",
        &json!({"name": "artifact-identity-pack"}),
    )
    .await;
    assert_eq!(
        pack.status, 201,
        "expected CreateSecurityRequirementPack to answer 201, got {} ({}) with {}",
        pack.status, pack.error_code, pack.body
    );
    let pack = pack.body;
    let domain = ok(
        &router,
        "CreateTargetDomain",
        &json!({
            "targetDomainName": "artifact-identity.example.com",
            "verificationMethod": VERIFICATION_METHOD_DNS_TXT,
        }),
        "a target domain beside the artifacts",
    )
    .await;
    let others: Vec<String> = spaces
        .iter()
        .cloned()
        .chain(member(&pack, "packId"))
        .chain(member(&domain, "targetDomainId"))
        .collect();
    for id in &minted {
        assert!(
            !others.contains(id),
            "the artifactId {id:?} is also the id of another resource of this account \
             ({others:?})"
        );
        for prefix in [
            AGENT_SPACE_ID_PREFIX,
            SECURITY_REQUIREMENT_PACK_ID_PREFIX,
            TARGET_DOMAIN_ID_PREFIX,
        ] {
            assert!(
                !id.starts_with(prefix),
                "the artifactId {id:?} also carries the {prefix:?} class prefix, so the \
                 two classes cannot be told apart"
            );
        }
    }
}

/// One operation of the artifact sub-API, with the request body for one `agentSpaceId`.
type OperationRow = (&'static str, fn(&str) -> Value);

/// RULE: an operation of a space's ARTIFACTS that cannot resolve the `agentSpaceId` it
/// was pointed at answers `ResourceNotFoundException` (HTTP 404) whose message is the
/// artifact path's fixed text with that `agentSpaceId` interpolated.
#[tokio::test]
async fn an_artifact_operation_names_the_agent_space_it_cannot_resolve() {
    let router = emulator();
    let stored = create_agent_space(&router, "artifact-refusal-space").await;
    let doomed = create_agent_space(&router, "artifact-refusal-doomed").await;
    let upload = Upload {
        case: "the artifact the stored space holds",
        space: 0,
        file_name: "held.txt",
        artifact_type: "TXT",
        content: b"held here",
    };
    let held = add(&router, &stored, &upload).await;
    let held = member(&held.body, "artifactId").expect("the upload minted an artifactId");
    ok(
        &router,
        "DeleteAgentSpace",
        &json!({"agentSpaceId": &doomed}),
        "the space the case deletes",
    )
    .await;

    // The rows differ along the FORM of the id that resolves to nothing: the shape of a
    // minted id whose body nothing minted, a readable label, another script, the stored
    // id with a suffix, and the id of an agent space a delete removed.
    let unknown_spaces: Vec<(&str, String)> = vec![
        (
            "an id of the class form nothing minted",
            format!("{AGENT_SPACE_ID_PREFIX}00000000-0000-0000-0000-000000000000"),
        ),
        ("a readable label", "no-such-agent-space".to_string()),
        ("an id in another script", "неизвестно".to_string()),
        ("the stored id with a suffix", format!("{stored}0")),
        ("the id of a deleted agent space", doomed.clone()),
    ];

    // Every operation of this service that resolves an `agentSpaceId` to reach the
    // artifacts it holds, with the rest of its required members filled in so the row is
    // about that lookup alone.
    let operations: Vec<OperationRow> = vec![
        (
            "AddArtifact",
            |id| {
                json!({
                    "agentSpaceId": id,
                    "fileName": "upload.txt",
                    "artifactType": "TXT",
                    "artifactContent": aws_smithy_types::base64::encode(b"artifact bytes"),
                })
            },
        ),
        (
            "GetArtifact",
            |id| json!({"agentSpaceId": id, "artifactId": "a-held"}),
        ),
        (
            "DeleteArtifact",
            |id| json!({"agentSpaceId": id, "artifactId": "a-held"}),
        ),
        ("ListArtifacts", |id| json!({"agentSpaceId": id})),
        (
            "BatchGetArtifactMetadata",
            |id| json!({"agentSpaceId": id, "artifactIds": ["a-held"]}),
        ),
    ];

    for (operation, request_for) in operations {
        for (form, unknown) in &unknown_spaces {
            let case = format!("{operation} with {form}");
            let answer = call(&router, operation, &request_for(unknown)).await;
            assert_eq!(
                (answer.status, answer.error_code.as_str()),
                (404, "ResourceNotFoundException"),
                "case {case:?}: expected 404 ResourceNotFoundException, got {} ({}) with {}",
                answer.status,
                answer.error_code,
                answer.body
            );
            let message = member(&answer.body, "message").unwrap_or_default();
            let expected = agent_space_not_found(unknown);
            assert_eq!(
                message, expected,
                "case {case:?}: expected {expected:?}, got {message:?}"
            );
            assert!(
                message.contains(unknown.as_str()),
                "case {case:?}: expected the refusal to name the rejected agentSpaceId \
                 {unknown:?}, got {message:?}"
            );
        }
    }

    // The refusals were about the lookup and nothing else: the artifact of the space the
    // account does hold still reads back under its own id.
    let read = get(&router, &stored, &held).await;
    assert_eq!(
        read.status, 200,
        "expected the stored artifact to survive the refusals, got {} ({}) with {}",
        read.status, read.error_code, read.body
    );
}
