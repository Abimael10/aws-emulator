// SPDX-License-Identifier: CC0-1.0

//! WHAT the temporary credentials Amazon GameLift Servers hands a caller for a build upload
//! may do: the upload they were vended for, and nothing else.
//!
//! RULE: a credential this service vends for an upload is SCOPED to that one upload — the
//! upload of the object the storage location names is served with it, and every other call
//! made with it, a read of the very object just written included, is refused HTTP 403
//! `AccessDenied` in the sentence IAM words for a request no session policy allows. The
//! scope is the whole of what the credential may do, because the identity it stands on is
//! bounded by no document of its own.
//!
//! The rows differ along what that rule generalises over: which operation vended the
//! credential (`CreateBuild`, which hands one with the build, and
//! `RequestUploadCredentials`, which refreshes it), which call is then made with it (a
//! write, a read, a delete, a listing of the bucket), and which object the call addresses
//! (the build's own key, and another build's key in the same service-owned bucket). The
//! recorded case — a read-back of the key just uploaded — is one row of that table.
//!
//! The harness (`GameLift`, `s3_as`, `caller_identity`) is the one `hosting_loop` drives the
//! production dispatch path with, with Amazon S3 and the token service mounted on the same
//! engine the way the router mounts every service.

use serde_json::{json, Value};

use super::hosting_loop::{GameLift, REGION, S3_CALLER_KEY_ID};

/// The code Amazon S3 answers an authorization refusal with, and the status it carries
/// (Amazon S3 "Error responses": `AccessDenied`, HTTP 403).
const ACCESS_DENIED: &str = "AccessDenied";
/// See [`ACCESS_DENIED`].
const ACCESS_DENIED_STATUS: u16 = 403;

/// The build files a row uploads. A scope is about which object a credential may reach, not
/// about what is in it, so the payload only has to differ per build for a read-back to say
/// whose files it served.
fn build_files(ordinal: usize) -> Vec<u8> {
    format!("gamelift-build-payload-{ordinal}").into_bytes()
}

/// One build created without a storage location of its own: the id, the service-owned
/// location its files belong at, and the access key id of the credentials vended with it.
struct Upload {
    build_id: String,
    bucket: String,
    key: String,
    access_key_id: String,
}

impl Upload {
    /// The path an object call addresses this build's own key by.
    fn path(&self) -> String {
        format!("/{}/{}", self.bucket, self.key)
    }

    /// The ARN a policy — and a refusal — names this build's object by.
    fn object_arn(&self) -> String {
        format!("arn:aws:s3:::{}/{}", self.bucket, self.key)
    }

    /// The ARN a refusal names the bucket holding it by.
    fn bucket_arn(&self) -> String {
        format!("arn:aws:s3:::{}", self.bucket)
    }
}

/// WHICH operation vended the credentials a row signs with: the two that hand them out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Vend {
    /// `CreateBuild`, which "returns an Amazon S3 location with temporary access
    /// credentials".
    Create,
    /// `RequestUploadCredentials`, which "retrieves a fresh set of credentials for use when
    /// uploading a new set of game build files".
    Refresh,
}

/// Create a build with no storage location of its own and read the vend off the response.
async fn created(gamelift: &GameLift, name: &str) -> Upload {
    let response = gamelift
        .ok(
            "CreateBuild",
            json!({"Name": name, "OperatingSystem": "AMAZON_LINUX_2023"}),
        )
        .await;
    let build_id = response["Build"]["BuildId"]
        .as_str()
        .expect("CreateBuild must report a BuildId")
        .to_string();
    vended(&build_id, &response, "CreateBuild")
}

/// Refresh the credentials of a stored build and read the fresh vend off the response.
async fn refreshed(gamelift: &GameLift, build_id: &str) -> Upload {
    let response = gamelift
        .ok("RequestUploadCredentials", json!({"BuildId": build_id}))
        .await;
    vended(build_id, &response, "RequestUploadCredentials")
}

/// The vend one response reports: its storage location, and the access key id of the
/// credentials it hands back.
fn vended(build_id: &str, response: &Value, call: &str) -> Upload {
    let location = &response["StorageLocation"];
    let access_key_id = response["UploadCredentials"]["AccessKeyId"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        !access_key_id.is_empty(),
        "case {call}: the vend must hand back an AccessKeyId to sign the upload with, got {}",
        response["UploadCredentials"]
    );
    Upload {
        build_id: build_id.to_string(),
        bucket: location["Bucket"]
            .as_str()
            .unwrap_or_else(|| panic!("case {call}: the vend must name a Bucket, got {location}"))
            .to_string(),
        key: location["Key"]
            .as_str()
            .unwrap_or_else(|| panic!("case {call}: the vend must name a Key, got {location}"))
            .to_string(),
        access_key_id,
    }
}

/// Upload `files` to `upload`'s own key with `upload`'s own credentials, which the scope
/// allows: the call the credentials were vended for.
async fn upload(gamelift: &GameLift, upload: &Upload, files: &[u8]) {
    let (status, body) = gamelift
        .s3_as(&upload.access_key_id, "PUT", &upload.path(), files)
        .await;
    assert_eq!(
        status,
        200,
        "case the upload of {}: the vended credentials must be allowed the upload they were \
         issued for, got {status} with body {}",
        upload.key,
        String::from_utf8_lossy(&body)
    );
}

/// The ARN `sts:GetCallerIdentity` reports for the credentials `access_key_id` belongs to:
/// the principal the emulator says they speak for, and the one a refusal of a request they
/// signed has to name.
async fn principal_of(gamelift: &GameLift, access_key_id: &str) -> String {
    let (status, document) = gamelift.caller_identity(access_key_id).await;
    assert_eq!(
        status, 200,
        "case {access_key_id}: the vended credentials must resolve a caller identity, got \
         {status} with body {document}"
    );
    let arn = element(&document, "Arn");
    assert!(
        !arn.is_empty(),
        "case {access_key_id}: the caller identity must carry an Arn, got {document}"
    );
    arn
}

/// The text of the first `<name>` element of an XML document, or the empty string when it
/// carries none. The awsQuery and restXml documents here are flat enough for this to be the
/// whole reading, and the entities XML escapes a message's own punctuation with are decoded,
/// so what this answers is the text the service wrote.
fn element(document: &str, name: &str) -> String {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let Some(start) = document.find(&open) else {
        return String::new();
    };
    let rest = &document[start + open.len()..];
    let Some(end) = rest.find(&close) else {
        return String::new();
    };
    rest[..end]
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// The whole sentence S3 refuses a request no session policy allows with: who was refused,
/// which IAM action, on which resource, and the clause naming the document that withheld the
/// grant (Amazon S3 "Error responses", and IAM's own published reason clause).
fn no_session_policy_allows(principal: &str, action: &str, resource: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: {action} on resource: \"{resource}\" \
         because no session policy allows the {action} action"
    )
}

/// One row of the refusal table: the call made with the vended credential, and the action and
/// resource the refusal of it names.
struct Refused {
    case: &'static str,
    method: &'static str,
    path: String,
    action: &'static str,
    resource: String,
}

/// Every call outside the upload the credential was vended for: its own key read back and
/// deleted, another build's key read and written, and the bucket listed.
fn refused_rows(subject: &Upload, neighbour: &Upload) -> Vec<Refused> {
    vec![
        Refused {
            // THE RECORDED CASE: the read-back of the very key just uploaded.
            case: "a read of the key the credential just wrote",
            method: "GET",
            path: subject.path(),
            action: "s3:GetObject",
            resource: subject.object_arn(),
        },
        Refused {
            case: "a delete of the key the credential just wrote",
            method: "DELETE",
            path: subject.path(),
            action: "s3:DeleteObject",
            resource: subject.object_arn(),
        },
        Refused {
            case: "a read of another build's key in the same bucket",
            method: "GET",
            path: neighbour.path(),
            action: "s3:GetObject",
            resource: neighbour.object_arn(),
        },
        Refused {
            case: "an upload to another build's key in the same bucket",
            method: "PUT",
            path: neighbour.path(),
            action: "s3:PutObject",
            resource: neighbour.object_arn(),
        },
        Refused {
            case: "a listing of the service-owned bucket",
            method: "GET",
            path: format!("/{}?list-type=2", subject.bucket),
            action: "s3:ListBucket",
            resource: subject.bucket_arn(),
        },
    ]
}

/// The credentials either operation vends authorise the upload they were issued for and
/// nothing else: every other call made with them is refused 403 `AccessDenied`, naming the
/// principal the token service reports for the credential, the action asked for, the resource
/// addressed, and the session policy as the document that withheld the grant.
#[tokio::test]
async fn vended_upload_credentials_authorize_that_upload_and_nothing_else() {
    let gamelift = GameLift::new();

    // A second build, uploaded with ITS own credentials: the neighbouring object the rows
    // below address to show the scope names one KEY and not the bucket it sits in.
    let neighbour = created(&gamelift, "upload-scope-neighbour").await;
    upload(&gamelift, &neighbour, &build_files(0)).await;

    for (ordinal, vend) in [Vend::Create, Vend::Refresh].iter().enumerate() {
        let subject = match vend {
            Vend::Create => created(&gamelift, "upload-scope-subject").await,
            Vend::Refresh => {
                let created = created(&gamelift, "upload-scope-refreshed").await;
                refreshed(&gamelift, &created.build_id).await
            }
        };
        assert_eq!(
            subject.bucket, neighbour.bucket,
            "case {vend:?}: the builds of one Region share the service's upload bucket, got \
             {} and {}",
            subject.bucket, neighbour.bucket
        );

        // The upload the credential was vended for is served.
        let files = build_files(ordinal + 1);
        upload(&gamelift, &subject, &files).await;

        // The location the credential is scoped to is the one the stored build reports, so
        // the scope and the persisted record can only agree.
        let described = gamelift
            .ok("DescribeBuild", json!({"BuildId": &subject.build_id}))
            .await;
        assert_eq!(
            described["Build"]["BuildId"],
            json!(&subject.build_id),
            "case {vend:?}: the build the credential was vended for must be the stored one, got {}",
            described["Build"]
        );

        let principal = principal_of(&gamelift, &subject.access_key_id).await;
        assert!(
            principal.starts_with(&format!(
                "arn:aws:sts::{}:",
                engine::emulator_account_id()
            )) && principal.contains(&subject.build_id),
            "case {vend:?}: the credential must speak for a session of this account issued for \
             the build it was vended for, got {principal:?}"
        );

        for row in refused_rows(&subject, &neighbour) {
            let (status, body) = gamelift
                .s3_as(&subject.access_key_id, row.method, &row.path, b"")
                .await;
            let document = String::from_utf8_lossy(&body).to_string();
            assert_eq!(
                (status, element(&document, "Code").as_str()),
                (ACCESS_DENIED_STATUS, ACCESS_DENIED),
                "case {vend:?} / {}: expected {ACCESS_DENIED_STATUS} {ACCESS_DENIED}, got \
                 {status} with body {document}",
                row.case
            );
            assert_eq!(
                element(&document, "Message"),
                no_session_policy_allows(&principal, row.action, &row.resource),
                "case {vend:?} / {}: expected the session-policy refusal, got {document}",
                row.case
            );
        }

        // The refusals above are about the CREDENTIAL's scope and nothing else: the same
        // object reads back for the account's own credential, carrying the bytes the scoped
        // upload wrote.
        let (status, served) = gamelift
            .s3_as(S3_CALLER_KEY_ID, "GET", &subject.path(), b"")
            .await;
        assert_eq!(
            (status, served.as_ref()),
            (200, files.as_slice()),
            "case {vend:?}: the uploaded files must be there for a credential the scope does \
             not bound, got {status} with {} bytes",
            served.len()
        );
    }
}

/// Each vend is its own scope: the credentials a build is handed cannot upload another
/// build's files, in whichever Region either build was created.
///
/// The rows differ in the Region the builds are created in, which is what names the
/// service-owned bucket — so the rule holds for two builds sharing a bucket and for two
/// builds in different ones.
#[tokio::test]
async fn one_builds_credentials_cannot_upload_another_builds_files() {
    let gamelift = GameLift::new();
    for region in [REGION, "eu-central-1"] {
        let first = created_in(&gamelift, region, "cross-upload-first").await;
        let second = created_in(&gamelift, region, "cross-upload-second").await;
        let principal = principal_of(&gamelift, &first.access_key_id).await;

        let (status, body) = gamelift
            .s3_as(
                &first.access_key_id,
                "PUT",
                &second.path(),
                &build_files(7),
            )
            .await;
        let document = String::from_utf8_lossy(&body).to_string();
        assert_eq!(
            (status, element(&document, "Code").as_str()),
            (ACCESS_DENIED_STATUS, ACCESS_DENIED),
            "case {region}: one build's credentials must not upload another build's files, got \
             {status} with body {document}"
        );
        assert_eq!(
            element(&document, "Message"),
            no_session_policy_allows(&principal, "s3:PutObject", &second.object_arn()),
            "case {region}: expected the session-policy refusal, got {document}"
        );

        // Each build's OWN upload is still served, so what the rows above prove is the scope
        // and not a credential that can do nothing.
        upload(&gamelift, &first, &build_files(8)).await;
        upload(&gamelift, &second, &build_files(9)).await;
    }
}

/// [`created`] for a build created in `region`: the emulator serves a request in the Region
/// of its SigV4 credential scope, and the service-owned bucket is that Region's.
async fn created_in(gamelift: &GameLift, region: &str, name: &str) -> Upload {
    let response = gamelift
        .ok_in(
            region,
            "CreateBuild",
            json!({"Name": name, "OperatingSystem": "AMAZON_LINUX_2023"}),
        )
        .await;
    let build_id = response["Build"]["BuildId"]
        .as_str()
        .expect("CreateBuild must report a BuildId")
        .to_string();
    vended(&build_id, &response, "CreateBuild")
}
