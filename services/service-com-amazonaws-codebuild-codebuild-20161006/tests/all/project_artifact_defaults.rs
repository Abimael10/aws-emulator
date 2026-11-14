// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the output-artifact configuration a stored build project
//! reports, driven over the wire the router serves through the harness
//! [`super::project_build_lifecycle`] owns.
//!
//! The rule under test: a project's `artifacts` are STORED with the members CodeBuild
//! supplies rather than the client — the output artifact's `name`, which is the project's
//! own name when the request names none, `packaging` resolved to `NONE` and
//! `encryptionDisabled` to `false` — so the create's answer and every later read report
//! the destination a build of the project would upload to, whatever `ArtifactsType` it
//! is. A member the request DID carry is stored as sent, and an update that replaces the
//! artifacts resolves the replacement the same way the create resolved what it replaced.
//!
//! The rows span what the rule generalises over: every published `ArtifactsType`
//! (`smithy_query(query="shape",shape="ArtifactsType")`: CODEPIPELINE | S3 |
//! NO_ARTIFACTS), both published `ArtifactPackaging` values (NONE | ZIP), both values of
//! the `encryptionDisabled` flag, a named and an unnamed artifact, and the destination
//! members that travel beside them.

use serde_json::{json, Value};

use crate::project_build_lifecycle::{create_project, emulator};
// The sibling suite owns the rest of the same resolution rule (the cache, the key, the
// timeouts, the environment and the source flags), so its project reader and its update
// helper are shared rather than copied.
use crate::project_effective_configuration::{fetch_project, update_project};
use crate::read_response_shape::at;

/// The `ArtifactPackaging` a destination that names none is stored with, from the default
/// AWS documents on `ProjectArtifacts$packaging`: "NONE: CodeBuild creates in the output
/// bucket a folder that contains the build output. This is the default if `packaging` is
/// not specified."
const DEFAULT_PACKAGING: &str = "NONE";

/// One row: the case, the `artifacts` the request carries, and the members the stored
/// artifacts must state — `None` for the artifact name meaning "the project's own name".
type Row = (&'static str, Value, Vec<(&'static str, Value)>);

/// The rows, each differing from the others along the artifacts type, which of the
/// resolved members the request names, and the destination members beside them.
fn rows() -> Vec<Row> {
    vec![
        (
            // The artifacts type whose output location CodePipeline manages: the members
            // are still resolved, because a read reports the configuration rather than
            // only what the request carried.
            "artifacts CodePipeline manages, nothing but the type",
            json!({"type": "CODEPIPELINE"}),
            vec![
                ("type", json!("CODEPIPELINE")),
                ("packaging", json!(DEFAULT_PACKAGING)),
                ("encryptionDisabled", json!(false)),
            ],
        ),
        (
            "no artifacts at all, nothing but the type",
            json!({"type": "NO_ARTIFACTS"}),
            vec![
                ("type", json!("NO_ARTIFACTS")),
                ("packaging", json!(DEFAULT_PACKAGING)),
                ("encryptionDisabled", json!(false)),
            ],
        ),
        (
            "a bucket destination naming only its bucket",
            json!({"type": "S3", "location": "output-bucket"}),
            vec![
                ("type", json!("S3")),
                ("location", json!("output-bucket")),
                ("packaging", json!(DEFAULT_PACKAGING)),
                ("encryptionDisabled", json!(false)),
            ],
        ),
        (
            "a bucket destination that names the output artifact itself",
            json!({"type": "S3", "location": "output-bucket", "name": "MyArtifact.zip"}),
            vec![
                ("name", json!("MyArtifact.zip")),
                ("packaging", json!(DEFAULT_PACKAGING)),
                ("encryptionDisabled", json!(false)),
            ],
        ),
        (
            // The other published ArtifactPackaging value, and the flag set the way the
            // service would not default it.
            "a zipped, unencrypted destination",
            json!({
                "type": "S3",
                "location": "output-bucket/team",
                "packaging": "ZIP",
                "encryptionDisabled": true,
            }),
            vec![
                ("packaging", json!("ZIP")),
                ("encryptionDisabled", json!(true)),
                ("location", json!("output-bucket/team")),
            ],
        ),
        (
            // `name` of "/" is the destination's documented way of asking for the root of
            // the output bucket, so it is a name the request DID carry.
            "a destination asking for the root of its bucket",
            json!({
                "type": "S3",
                "location": "output-bucket",
                "name": "/",
                "namespaceType": "BUILD_ID",
                "path": "MyArtifacts",
            }),
            vec![
                ("name", json!("/")),
                ("namespaceType", json!("BUILD_ID")),
                ("path", json!("MyArtifacts")),
                ("packaging", json!(DEFAULT_PACKAGING)),
                ("encryptionDisabled", json!(false)),
            ],
        ),
        (
            "an encrypted, explicitly unpackaged destination",
            json!({
                "type": "S3",
                "location": "output-bucket",
                "packaging": "NONE",
                "encryptionDisabled": false,
                "overrideArtifactName": true,
            }),
            vec![
                ("packaging", json!("NONE")),
                ("encryptionDisabled", json!(false)),
                ("overrideArtifactName", json!(true)),
            ],
        ),
    ]
}

/// The artifact name a row expects: the one its request carried, or the project's own
/// name when it carried none.
fn expected_name(request: &Value, project: &str) -> Value {
    match request["name"].as_str() {
        Some(name) => json!(name),
        None => json!(project),
    }
}

/// RULE: a create stores — and therefore every read reports — the artifacts configuration
/// with the members the service resolved, and the members the request carried as sent.
#[tokio::test]
async fn a_stored_project_states_the_artifact_members_the_service_resolved() {
    for (index, (case, artifacts, expected)) in rows().into_iter().enumerate() {
        let router = emulator();
        let name = format!("artifact-defaults-{index:02}");
        let created = create_project(&router, &name, json!({"artifacts": artifacts.clone()})).await;

        assert_eq!(
            at(&created, "artifacts.name"),
            expected_name(&artifacts, &name),
            "case {case:?} / CreateProject: expected the artifact name {:?}, got {created}",
            expected_name(&artifacts, &name)
        );
        for (member, want) in &expected {
            let path = format!("artifacts.{member}");
            assert_eq!(
                &at(&created, &path),
                want,
                "case {case:?} / CreateProject: expected {path} = {want}, got {:?}",
                at(&created, &path)
            );
        }

        // The resolution is PERSISTED, not computed per answer: the read reports the very
        // record the create stored.
        let fetched = fetch_project(&router, &name).await;
        assert_eq!(
            fetched, created,
            "case {case:?}: BatchGetProjects must report the stored project verbatim, got \
             {fetched}"
        );

        // An update that names nothing but the project leaves the resolved members where
        // they were, so a client that round-trips a project cannot lose them.
        let updated = update_project(&router, &name, json!({})).await;
        assert_eq!(
            at(&updated, "artifacts"),
            at(&created, "artifacts"),
            "case {case:?} / UpdateProject: the artifacts must stay as stored, got {updated}"
        );
    }
}

/// RULE, for a replacement: the artifacts `UpdateProject` hands over are stored with the
/// members the service supplies resolved, exactly as the create resolved the ones they
/// replace — so a project updated with a bare artifacts block does not lose them.
#[tokio::test]
async fn an_update_resolves_the_artifact_members_of_the_destination_it_replaces() {
    for (index, (case, artifacts, expected)) in rows().into_iter().enumerate() {
        let router = emulator();
        let name = format!("artifact-replaced-{index:02}");
        // The project starts from a destination with every resolved member explicitly set
        // away from what the service would default, so a reappearing default can only
        // have come from the resolution of the REPLACEMENT.
        create_project(
            &router,
            &name,
            json!({"artifacts": {
                "type": "S3",
                "location": "configured-bucket",
                "name": "configured-artifact",
                "packaging": "ZIP",
                "encryptionDisabled": true,
            }}),
        )
        .await;

        let updated = update_project(&router, &name, json!({"artifacts": artifacts.clone()})).await;
        assert_eq!(
            at(&updated, "artifacts.name"),
            expected_name(&artifacts, &name),
            "case {case:?} / UpdateProject: expected the artifact name {:?}, got {updated}",
            expected_name(&artifacts, &name)
        );
        for (member, want) in &expected {
            let path = format!("artifacts.{member}");
            assert_eq!(
                &at(&updated, &path),
                want,
                "case {case:?} / UpdateProject: expected {path} = {want}, got {:?}",
                at(&updated, &path)
            );
        }

        let fetched = fetch_project(&router, &name).await;
        assert_eq!(
            fetched, updated,
            "case {case:?}: BatchGetProjects must report the updated record, got {fetched}"
        );
    }
}

/// RULE, on where the derived name COMES FROM: it is the name the request used, so two
/// projects created with the same bare artifacts block carry different artifact names.
///
/// The names differ in length and in the characters `ProjectName`'s pattern admits, so
/// the assertion cannot pass on a value fitted to one of them.
#[tokio::test]
async fn the_derived_artifact_name_is_the_name_the_request_used() {
    let router = emulator();
    // `ProjectName` is `@length(min: 2, max: 150)` with pattern
    // `^[A-Za-z0-9][A-Za-z0-9\-_]{1,149}$`, so these span its shortest name, a mixed one
    // carrying both punctuation characters it admits, and its longest.
    let names = vec![
        "ab".to_string(),
        "Derived_Artifact-Name".to_string(),
        format!("p{}", "x".repeat(149)),
    ];
    for name in &names {
        let name = name.as_str();
        let created = create_project(
            &router,
            name,
            json!({"artifacts": {"type": "CODEPIPELINE"}}),
        )
        .await;
        assert_eq!(
            at(&created, "artifacts.name"),
            json!(name),
            "case {name:?}: the artifact name must be the project's own, got {created}"
        );
        let fetched = fetch_project(&router, name).await;
        assert_eq!(
            at(&fetched, "artifacts.name"),
            json!(name),
            "case {name:?}: the read must report the derived artifact name, got {fetched}"
        );
    }
}
