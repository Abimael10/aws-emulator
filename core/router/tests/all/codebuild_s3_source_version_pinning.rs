// SPDX-License-Identifier: CC0-1.0

//! WHICH bytes of an S3 source object a CodeBuild build runs on, across the one emulator.
//!
//! RULE: a build whose source is an object of the account's object store downloads that
//! object and unpacks it into the primary source root before the first command of the
//! buildspec runs, so a command that reads a file of the repository reads it out of that
//! archive — and WHICH version it reads is what the build's `sourceVersion` names:
//! `StartBuildInput$sourceVersion` is "for Amazon S3: the version ID of the object that
//! represents the build input ZIP file to use", a start that names none takes the version the
//! project names (`CreateProject$sourceVersion`) and otherwise the object as it stands, and a
//! build that names a version the store serves nothing for has nothing to run: the phase that
//! fetches the source fails, naming the object, and no command of the buildspec runs. Because
//! the version the build resolved is the build's own record, a `RetryBuild` — which "restarts
//! a build" from that record — runs on the same bytes as the build it retries however the
//! object has changed since.
//!
//! Three services have to agree for that to hold — Amazon S3 holds the versioned object, IAM
//! holds the project's service role, and CodeBuild runs the build — so the suite lives here:
//! the CodeBuild crate's own suites have no object store to stage an archive in.
//! `codebuild_s3_build_output.rs` pins what a build PUTS in the store and that an unpinned
//! build runs on the archive its project names; what is pinned here is the VERSION the
//! download resolves, which the archive-reading alone cannot show.
//!
//! The rows differ along what the rule generalises over: whether the start pins a version,
//! which of the object's versions it pins, whether the pin comes from the request or from the
//! project, whether the pinned version is one the store holds, and whether the build is a
//! fresh start or a retry of one.

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The harness for a CodeBuild/S3/IAM emulator over one engine, written ONCE for this test
// crate in the sibling suite that staged it first: the router assembly, the signed dispatch,
// the CodeBuild wire, the bucket create, the service role and the settle poll. `tests/all` is
// one crate root, so this suite shares them rather than growing a second copy.
use super::codebuild_s3_build_output::{
    codebuild, codebuild_role, create_bucket, emulator, send, settled,
};
use crate::query_wire::{blocks_of, elements};

/// The namespace Amazon S3's own request documents carry.
const S3_XMLNS: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

/// The key every staged archive is written to, so two writes of it are two VERSIONS of one
/// object rather than two objects.
const SOURCE_KEY: &str = "source.zip";

/// The file each staged archive holds, which the reader project's buildspec reads.
const PAYLOAD_FILE: &str = "payload.txt";

/// The name the reader project exports what it read under.
const EXPORTED_NAME: &str = "SOURCE_PAYLOAD";

/// What the first staged version of the archive holds.
const PAYLOAD_ONE: &str = "payload-of-the-first-version";

/// What the second one holds, written over the same key.
const PAYLOAD_TWO: &str = "payload-of-the-second-version";

/// A version id shaped like one Amazon S3 issues that no write below ever minted: 32
/// characters drawn from the alphanumerics an opaque `versionId` is written in
/// (`smithy_query(query="shape", shape="ObjectVersionId")` constrains none, so the shape is
/// S3's own published token). A malformed token would be refused as an ARGUMENT instead,
/// which is a different answer from a version the store holds nothing for.
const UNMINTED_VERSION: &str = "3nZ9Lh0ptXoBPinnedButNeverMinted";

/// One call to Amazon S3's own REST wire, answered with the status and the XML body.
async fn s3(router: &MultiServiceRouter, method: &str, path: &str, body: &[u8]) -> (u16, String) {
    let (status, answered) = send(
        router,
        "s3",
        method,
        path,
        &[("content-type", "application/xml".to_string())],
        Bytes::copy_from_slice(body),
    )
    .await;
    (status, String::from_utf8_lossy(&answered).to_string())
}

/// Create `bucket` and turn its versioning on, so two writes of one key are two versions a
/// build can be pinned to.
async fn versioned_bucket(router: &MultiServiceRouter, bucket: &str) {
    create_bucket(router, bucket).await;
    let (status, body) = s3(
        router,
        "PUT",
        &format!("/{bucket}?versioning"),
        format!(
            "<VersioningConfiguration xmlns=\"{S3_XMLNS}\"><Status>Enabled</Status>\
             </VersioningConfiguration>"
        )
        .as_bytes(),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: the bucket {bucket:?} must take versioning, got {status} with {body}"
    );
}

/// The version ids Amazon S3 reports for `key`, in the order its own version listing reports
/// them (`GET /<bucket>?versions`), each paired with whether it is the key's current version.
async fn versions_of(router: &MultiServiceRouter, bucket: &str, key: &str) -> Vec<(String, bool)> {
    let (status, body) = s3(router, "GET", &format!("/{bucket}?versions"), &[]).await;
    assert_eq!(
        status, 200,
        "ListObjectVersions({bucket:?}) must succeed, got {status} with {body}"
    );
    // Read per `<Version>` block, so a key stays paired with its OWN id and latest flag.
    blocks_of(&body, "Version")
        .into_iter()
        .filter(|block| elements(block, "Key").first().map(String::as_str) == Some(key))
        .filter_map(|block| {
            let id = elements(block, "VersionId").first().cloned()?;
            let latest = elements(block, "IsLatest").first().cloned();
            Some((id, latest.as_deref() == Some("true")))
        })
        .collect()
}

/// Create one project and assert it was stored.
async fn create_project(router: &MultiServiceRouter, name: &str, body: Value) {
    let (status, created) = codebuild(router, "CreateProject", body).await;
    assert_eq!(
        status, 200,
        "setup: CreateProject({name:?}) must succeed, got {status} with {created}"
    );
}

/// The members every project below carries beside its own source and artifacts.
fn environment(role_arn: &str) -> Value {
    json!({
        "environment": {
            "type": "LINUX_CONTAINER",
            "image": "aws/codebuild/standard:7.0",
            "computeType": "BUILD_GENERAL1_SMALL",
        },
        "serviceRole": role_arn,
    })
}

/// `extra`'s members merged into `body` — both are JSON objects, so a mis-shaped one panics
/// rather than passing `body` through unchanged, which would assert a row against a request it
/// never sent.
fn merged(body: Value, extra: Value) -> Value {
    match (body, extra) {
        (Value::Object(mut target), Value::Object(members)) => {
            target.extend(members);
            Value::Object(target)
        }
        (body, extra) => panic!(
            "a request and the members merged into it are JSON objects, got {body} and {extra}"
        ),
    }
}

/// Write one ZIP archive holding `payload` to `bucket`/[`SOURCE_KEY`], as a build of its own.
///
/// The archive is staged by a CodeBuild build whose artifacts are packaged `ZIP` — exactly
/// the form a build's S3 source takes — so the suite stages a real archive without writing
/// one by hand, and each staging is one more VERSION of that one key.
async fn stage_archive(
    router: &MultiServiceRouter,
    role_arn: &str,
    bucket: &str,
    index: usize,
    payload: &str,
) {
    let project = format!("source-stage-{index}");
    let buildspec = format!(
        "version: 0.2\nphases:\n  build:\n    commands:\n      \
         - \"printf '%s' '{payload}' > {PAYLOAD_FILE}\"\nartifacts:\n  files:\n    \
         - {PAYLOAD_FILE}\n"
    );
    create_project(
        router,
        &project,
        merged(
            json!({
                "name": project,
                "source": {"type": "NO_SOURCE", "buildspec": buildspec},
                "artifacts": {
                    "type": "S3",
                    "location": bucket,
                    "name": SOURCE_KEY,
                    "packaging": "ZIP",
                },
            }),
            environment(role_arn),
        ),
    )
    .await;
    let staged = settled_build(router, &project, json!({})).await;
    assert_eq!(
        staged["buildStatus"],
        json!("SUCCEEDED"),
        "setup: the build staging {payload:?} must succeed, got {staged}"
    );
}

/// Create the project whose builds READ the staged archive: its source is the object, and its
/// buildspec exports what it read out of the primary source root.
async fn create_reader(
    router: &MultiServiceRouter,
    name: &str,
    role_arn: &str,
    bucket: &str,
    extra: Value,
) {
    let buildspec = format!(
        "version: 0.2\nenv:\n  exported-variables:\n    - {EXPORTED_NAME}\nphases:\n  \
         build:\n    commands:\n      - 'export {EXPORTED_NAME}=\"$(cat {PAYLOAD_FILE})\"'\n"
    );
    create_project(
        router,
        name,
        merged(
            merged(
                json!({
                    "name": name,
                    "source": {
                        "type": "S3",
                        "location": format!("{bucket}/{SOURCE_KEY}"),
                        "buildspec": buildspec,
                    },
                    "artifacts": {"type": "NO_ARTIFACTS"},
                }),
                environment(role_arn),
            ),
            extra,
        ),
    )
    .await;
}

/// Start a build of `project` with `extra`'s members merged into the request, and answer the
/// id CodeBuild minted for it.
async fn start_build(router: &MultiServiceRouter, project: &str, extra: Value) -> String {
    let body = merged(json!({"projectName": project}), extra);
    let (status, started) = codebuild(router, "StartBuild", body.clone()).await;
    assert_eq!(
        status, 200,
        "StartBuild({body}): expected 200, got {status} with {started}"
    );
    started["build"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("StartBuild({body}) must mint a build, got {started}"))
        .to_string()
}

/// Start a build of `project` and poll until it reports itself complete.
async fn settled_build(router: &MultiServiceRouter, project: &str, extra: Value) -> Value {
    let id = start_build(router, project, extra).await;
    settled(router, &id).await
}

/// The value a settled build reports for the exported variable `name` — `None` when it
/// reports no entry under that name at all, which is a different answer from an entry whose
/// value is empty.
fn exported(build: &Value, name: &str) -> Option<String> {
    build["exportedEnvironmentVariables"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|variable| variable["name"] == json!(name))
        .map(|variable| variable["value"].as_str().unwrap_or_default().to_string())
}

/// What the phase `phase_type` reports: its `phaseStatus`, and the `statusCode` and `message`
/// of the first context it carries — each an empty string when the build reports none.
fn phase_context(build: &Value, phase_type: &str) -> (String, String, String) {
    let phase = build["phases"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|phase| phase["phaseType"] == json!(phase_type))
        .cloned()
        .unwrap_or(Value::Null);
    let text = |value: &Value| value.as_str().unwrap_or_default().to_string();
    (
        text(&phase["phaseStatus"]),
        text(&phase["contexts"][0]["statusCode"]),
        text(&phase["contexts"][0]["message"]),
    )
}

/// The two versions of the one source object, as `(first, second)` — staged in order, so the
/// second is the key's current version and the first is only reachable by naming it.
async fn two_staged_versions(
    router: &MultiServiceRouter,
    role_arn: &str,
    bucket: &str,
) -> (String, String) {
    versioned_bucket(router, bucket).await;
    stage_archive(router, role_arn, bucket, 1, PAYLOAD_ONE).await;
    let after_first = versions_of(router, bucket, SOURCE_KEY).await;
    assert_eq!(
        after_first.len(),
        1,
        "setup: the first write must leave one version of {SOURCE_KEY:?}, got {after_first:?}"
    );
    let first = after_first[0].0.clone();

    stage_archive(router, role_arn, bucket, 2, PAYLOAD_TWO).await;
    let after_second = versions_of(router, bucket, SOURCE_KEY).await;
    assert_eq!(
        after_second.len(),
        2,
        "setup: the second write must leave two versions of {SOURCE_KEY:?}, got \
         {after_second:?}"
    );
    let second = after_second
        .iter()
        .find(|(id, latest)| *latest && *id != first)
        .map(|(id, _)| id.clone())
        .unwrap_or_else(|| {
            panic!(
                "setup: the second write must be the current version of {SOURCE_KEY:?}, got \
                 {after_second:?}"
            )
        });
    (first, second)
}

/// What a row's build comes to.
enum Outcome {
    /// Its commands ran over the unpacked source and read this out of it.
    Read(&'static str),
    /// No command ran: the phase that fetches the source failed, so the build exported
    /// nothing.
    SourceRefused,
}

impl Outcome {
    /// The build's terminal `buildStatus`.
    fn status(&self) -> &'static str {
        match self {
            Self::Read(_) => "SUCCEEDED",
            Self::SourceRefused => "FAILED",
        }
    }

    /// What the build reports for the exported name, or `None` for one that reports no entry
    /// under it.
    fn exported(&self) -> Option<&'static str> {
        match self {
            Self::Read(payload) => Some(payload),
            Self::SourceRefused => None,
        }
    }
}

/// One row of the start-pinning table.
struct Row {
    case: &'static str,
    /// The version the start pins, or `None` for a start that names none.
    pinned: Option<String>,
    /// What the build comes to.
    outcome: Outcome,
    /// Whether this row's build is RETRIED as well, which runs the retry on the settings the
    /// retried build resolved rather than on anything the retry names.
    retried: bool,
}

/// RULE: the version a build's `sourceVersion` names decides which bytes of the source object
/// its commands read — the pinned version however the object has changed since, the object as
/// it stands for a build that pins none, and nothing at all for a version the store holds
/// none of, which fails the phase that fetches the source. A RETRY runs on the version the
/// build it retries resolved, so it reads those same bytes.
#[tokio::test]
async fn a_builds_source_version_decides_which_bytes_its_commands_read() {
    let router = emulator();
    let role = codebuild_role(&router, "codebuild-pinned-source-role").await;
    let bucket = "codebuild-pinned-source-store";
    let (first, second) = two_staged_versions(&router, &role, bucket).await;

    let project = "codebuild-pinned-source-reader";
    create_reader(&router, project, &role, bucket, json!({})).await;

    let rows: Vec<Row> = vec![
        Row {
            case: "pinned to the version written first, which the object no longer holds",
            pinned: Some(first.clone()),
            outcome: Outcome::Read(PAYLOAD_ONE),
            retried: true,
        },
        Row {
            case: "pinned to the version written second",
            pinned: Some(second.clone()),
            outcome: Outcome::Read(PAYLOAD_TWO),
            retried: false,
        },
        Row {
            case: "no version pinned, so the object as it stands",
            pinned: None,
            outcome: Outcome::Read(PAYLOAD_TWO),
            retried: true,
        },
        Row {
            case: "pinned to a version the store holds none of",
            pinned: Some(UNMINTED_VERSION.to_string()),
            outcome: Outcome::SourceRefused,
            retried: false,
        },
    ];

    for row in &rows {
        let case = row.case;
        let extra = match row.pinned.as_deref() {
            Some(version) => json!({"sourceVersion": version}),
            None => json!({}),
        };
        let build = settled_build(&router, project, extra).await;
        assert_eq!(
            build["buildStatus"],
            json!(row.outcome.status()),
            "case {case:?}: expected buildStatus {}, got {build}",
            row.outcome.status()
        );
        let read = exported(&build, EXPORTED_NAME);
        assert_eq!(
            read.as_deref(),
            row.outcome.exported(),
            "case {case:?}: expected the source to hold {:?}, got {read:?}",
            row.outcome.exported()
        );
        let reported = build["sourceVersion"].clone();
        let wanted = row
            .pinned
            .as_deref()
            .map_or(Value::Null, |version| json!(version));
        assert_eq!(
            reported, wanted,
            "case {case:?}: the build must report the version it was started with, expected \
             {wanted}, got {reported}"
        );
        if matches!(row.outcome, Outcome::SourceRefused) {
            // A version the store served nothing for is the build's own declaration or the
            // account's own state rather than a command's, so the phase that fetches the
            // source fails carrying that classification and the OBJECT it was asked for, in
            // the S3 ARN form a source location is reported in.
            let (phase_status, status_code, message) = phase_context(&build, "DOWNLOAD_SOURCE");
            assert_eq!(
                (phase_status.as_str(), status_code.as_str()),
                ("FAILED", "CLIENT_ERROR"),
                "case {case:?}: expected the source phase to fail as a client error, got \
                 {phase_status:?}/{status_code:?} with {message:?}"
            );
            let address = format!("arn:aws:s3:::{bucket}/{SOURCE_KEY}");
            assert!(
                message.contains(&address),
                "case {case:?}: the phase must name the object {address:?}, got {message:?}"
            );
        }

        // A retry of this build is driven from the retried build's own record, so what it
        // reads is decided by the version THAT build resolved — the retry names none.
        if !row.retried {
            continue;
        }
        let id = build["id"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: the build must report an id, got {build}"));
        let (status, retried) = codebuild(&router, "RetryBuild", json!({"id": id})).await;
        assert_eq!(
            status, 200,
            "case {case:?}: RetryBuild({id:?}) expected 200, got {status} with {retried}"
        );
        let retry_id = retried["build"]["id"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: RetryBuild must mint a build, got {retried}"))
            .to_string();
        assert_ne!(
            retry_id, id,
            "case {case:?}: a retry is its own build, got {retried}"
        );
        let retry = settled(&router, &retry_id).await;
        assert_eq!(
            retry["buildStatus"],
            json!("SUCCEEDED"),
            "case {case:?}: the retry must succeed, got {retry}"
        );
        assert_eq!(
            retry["sourceVersion"], build["sourceVersion"],
            "case {case:?}: the retry carries the retried build's source version, got {retry}"
        );
        let read = exported(&retry, EXPORTED_NAME);
        assert_eq!(
            read.as_deref(),
            row.outcome.exported(),
            "case {case:?}: expected the retry to read {:?}, got {read:?}",
            row.outcome.exported()
        );
    }
}

/// RULE: the version a start names comes first, and a start that names none takes the one the
/// PROJECT names, so a project pinned to a version builds on that version's bytes until a
/// start names another.
#[tokio::test]
async fn a_project_pinned_to_a_version_builds_on_it_until_a_start_names_another() {
    let router = emulator();
    let role = codebuild_role(&router, "codebuild-project-pin-role").await;
    let bucket = "codebuild-project-pin-store";
    let (first, second) = two_staged_versions(&router, &role, bucket).await;

    let project = "codebuild-project-pin-reader";
    create_reader(
        &router,
        project,
        &role,
        bucket,
        json!({"sourceVersion": first.clone()}),
    )
    .await;

    // (case, the version the start pins, what its commands must read)
    let rows: Vec<(&str, Option<&str>, &str)> = vec![
        (
            "a start naming no version takes the project's own",
            None,
            PAYLOAD_ONE,
        ),
        (
            "a start naming a version overrides the project's",
            Some(second.as_str()),
            PAYLOAD_TWO,
        ),
    ];

    for (case, pinned, expected) in rows {
        let extra = match pinned {
            Some(version) => json!({"sourceVersion": version}),
            None => json!({}),
        };
        let build = settled_build(&router, project, extra).await;
        assert_eq!(
            build["buildStatus"],
            json!("SUCCEEDED"),
            "case {case:?}: the build must succeed, got {build}"
        );
        let read = exported(&build, EXPORTED_NAME);
        assert_eq!(
            read.as_deref(),
            Some(expected),
            "case {case:?}: expected the source to hold {expected:?}, got {read:?}"
        );
        let wanted = pinned.unwrap_or(first.as_str());
        assert_eq!(
            build["sourceVersion"],
            json!(wanted),
            "case {case:?}: the build must report the version it resolved, expected {wanted:?}, \
             got {build}"
        );
    }
}
