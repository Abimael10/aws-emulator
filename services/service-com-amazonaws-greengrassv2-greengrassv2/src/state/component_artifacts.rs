// SPDX-License-Identifier: CC0-1.0

//! Which artifacts a component version declares, and where the object each one names is
//! downloaded from.
//!
//! A component's artifacts are declared by its RECIPE: each platform manifest carries an
//! artifact list, and each entry of that list names its artifact's location
//! (`Manifests[].Artifacts[].Uri`, the IoT Greengrass component recipe reference). So the
//! recipe `CreateComponentVersion` persisted is the one place an artifact's existence is
//! decided, and nothing is held beside it — a version's artifacts cannot disagree with the
//! recipe `GetComponent` answers with.
//!
//! The name an artifact is asked for by is "the section of the URI after the scheme"
//! (`GetComponentVersionArtifact`'s `artifactName` member, whose example reads the name
//! `SomeArtifact.zip` out of the URI `greengrass:SomeArtifact.zip`). A location that names
//! an object also names it by the FILE name under that section, which is the name the
//! artifact is stored under on a core device — either spelling names the one artifact, as
//! either component ARN shape names the one component ([`super::addressed_component`]).

use super::*;

/// The recipe member one artifact entry names its location in.
const RECIPE_ARTIFACT_URI_KEY: &str = "Uri";

/// What an artifact URI naming an Amazon S3 object opens with: IoT Greengrass takes a
/// private component's artifacts from the account's own S3 objects, which is where the
/// download URL this service hands out leads.
const S3_URI_PREFIX: &str = "s3://";

/// Where one declared artifact's bytes are: the Amazon S3 object its URI names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactObject {
    /// The bucket the object is held in.
    pub bucket: String,
    /// The key it is held under.
    pub key: String,
}

impl ArtifactObject {
    /// The path the object is addressed at, as a path-style Amazon S3 URL lays it out —
    /// the path the download URL this service hands out carries, so the address announced
    /// and the object named cannot disagree.
    #[must_use]
    pub fn path(&self) -> String {
        format!("/{}/{}", self.bucket, self.key)
    }
}

/// The object the artifact `artifact_name` names in `recipe`, or `None` when that recipe
/// declares no downloadable artifact of that name.
///
/// The recipe bytes are the stored ones, in either wire format: YAML 1.2 is a superset of
/// JSON, so the one parser reads both ([`super::parse_recipe`]), and the bounds that parser
/// holds an untrusted document to apply here too.
#[must_use]
pub fn declared_artifact(recipe: &[u8], artifact_name: &str) -> Option<ArtifactObject> {
    let uri = declared_artifact_uris(recipe)
        .into_iter()
        .find(|uri| names_artifact(uri, artifact_name))?;
    artifact_object(&uri)
}

/// Every artifact location a stored recipe declares, in the order the document declares
/// them: each manifest's artifact list, manifest by manifest.
///
/// A document that cannot be read, or that declares no manifest or no artifact, declares no
/// location — which is the same answer as a document whose artifacts are all named
/// something else, and is why this cannot report an artifact a recipe does not carry.
fn declared_artifact_uris(recipe: &[u8]) -> Vec<String> {
    let Ok(text) = std::str::from_utf8(recipe) else {
        return Vec::new();
    };
    let Ok(document) = load_document(text) else {
        return Vec::new();
    };
    let manifests = document
        .as_hash()
        .and_then(|map| mapping_get(map, RECIPE_MANIFESTS_KEY))
        .and_then(::yaml_rust2::Yaml::as_vec);
    let Some(manifests) = manifests else {
        return Vec::new();
    };
    manifests
        .iter()
        .filter_map(|manifest| {
            let artifacts = mapping_get(manifest.as_hash()?, RECIPE_ARTIFACTS_KEY)?.as_vec()?;
            Some(
                artifacts
                    .iter()
                    .filter_map(|artifact| {
                        mapping_get(artifact.as_hash()?, RECIPE_ARTIFACT_URI_KEY)
                            .and_then(scalar_string)
                    })
                    .collect::<Vec<String>>(),
            )
        })
        .flatten()
        .collect()
}

/// Whether `artifact_name` names the artifact declared at `uri`: the section of the URI
/// after its scheme, or the file name at the end of that section.
fn names_artifact(uri: &str, artifact_name: &str) -> bool {
    let Some(section) = after_scheme(uri) else {
        return false;
    };
    section == artifact_name || file_name(section) == artifact_name
}

/// The section of `uri` after its scheme — the name the artifact is asked for by — or
/// `None` for a value that is not a URI with a scheme and something after it.
///
/// The separator a hierarchical URI writes between its scheme and its authority belongs to
/// neither, so `s3://bucket/key.zip` names `bucket/key.zip` just as `greengrass:a.zip`
/// names `a.zip`.
fn after_scheme(uri: &str) -> Option<&str> {
    let (scheme, rest) = uri.split_once(':')?;
    if scheme.is_empty() {
        return None;
    }
    let section = rest.strip_prefix("//").unwrap_or(rest);
    (!section.is_empty()).then_some(section)
}

/// The file name at the end of an artifact's section: the last path segment, which is the
/// name the artifact is stored under on a core device.
fn file_name(section: &str) -> &str {
    section
        .rsplit_once('/')
        .map_or(section, |(_, name)| name)
}

/// The object `uri` names, or `None` when it names no Amazon S3 object — a container image
/// reference, or an artifact of a store this account holds no object in, neither of which
/// has a download URL to hand out.
fn artifact_object(uri: &str) -> Option<ArtifactObject> {
    let (bucket, key) = uri.strip_prefix(S3_URI_PREFIX)?.split_once('/')?;
    (!bucket.is_empty() && !key.is_empty()).then(|| ArtifactObject {
        bucket: bucket.to_string(),
        key: key.to_string(),
    })
}

/// The sentence IoT Greengrass refuses an artifact read with when it finds no such
/// artifact: fixed text around the component the request addressed, the artifact name it
/// asked for and the account the search was made in.
///
/// ONE sentence for both halves of the search, as AWS words it: the component version the
/// ARN names may not exist, or it may exist and declare no artifact of that name, and the
/// refusal distinguishes neither — which is what the `or` in it says.
#[must_use]
pub fn artifact_not_found_message(
    component_name: &str,
    component_version: &str,
    artifact_name: &str,
) -> String {
    format!(
        "Component ({component_name}:{component_version}) or Artifact ({artifact_name}) for \
         account {} does not exist",
        account_id()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A recipe declaring `manifests`, each manifest a list of artifact URIs.
    fn recipe_with(manifests: &[&[&str]]) -> Vec<u8> {
        let mut document = String::from(
            "RecipeFormatVersion: '2020-01-25'\nComponentName: com.example.Artifacts\n\
             ComponentVersion: 1.0.0\nManifests:\n",
        );
        for artifacts in manifests {
            document.push_str("  - Platform:\n      os: linux\n    Artifacts:\n");
            for uri in *artifacts {
                document.push_str(&format!("      - Uri: {uri}\n"));
            }
        }
        document.into_bytes()
    }

    /// RULE: an artifact is the one its recipe declares, named by the section of its URI
    /// after the scheme or by the file name at the end of that section, and the object
    /// handed back is the one that URI names.
    #[test]
    fn a_declared_object_artifact_is_found_under_either_name() {
        let recipe = recipe_with(&[
            &["s3://artifact-bucket/components/first.zip"],
            &["s3://other-bucket/second.zip", "docker://registry/image:1"],
        ]);
        /// (case, the name asked for, the object it must name)
        type Row<'a> = (&'a str, &'a str, Option<(&'a str, &'a str)>);
        let rows: &[Row] = &[
            (
                "the whole section after the scheme",
                "artifact-bucket/components/first.zip",
                Some(("artifact-bucket", "components/first.zip")),
            ),
            (
                "the file name at the end of that section",
                "first.zip",
                Some(("artifact-bucket", "components/first.zip")),
            ),
            (
                "an artifact declared by a later manifest",
                "second.zip",
                Some(("other-bucket", "second.zip")),
            ),
            (
                "the same artifact by its whole section",
                "other-bucket/second.zip",
                Some(("other-bucket", "second.zip")),
            ),
            ("a name the recipe declares nowhere", "absent.zip", None),
            (
                "a name differing only in case",
                "First.zip",
                None,
            ),
            (
                "an artifact that names a container image rather than an object",
                "registry/image:1",
                None,
            ),
            ("the scheme alone", "s3://", None),
            ("the empty name", "", None),
        ];
        for (case, asked, expected) in rows {
            let found = declared_artifact(&recipe, asked);
            let expected = expected.map(|(bucket, key)| ArtifactObject {
                bucket: bucket.to_string(),
                key: key.to_string(),
            });
            assert_eq!(
                found, expected,
                "case {case:?}: expected {expected:?} for artifact {asked:?}, got {found:?}"
            );
        }
    }

    /// RULE: a recipe that declares no artifact list declares no artifact, whatever else it
    /// carries — including a document that cannot be read at all.
    #[test]
    fn a_recipe_without_artifacts_declares_none() {
        // (case, the stored recipe)
        let rows: Vec<(&str, Vec<u8>)> = vec![
            ("no Manifests member", b"ComponentName: a\n".to_vec()),
            ("a manifest with no Artifacts member", recipe_with(&[&[]])),
            (
                "an empty artifact list, as a stored recipe supplies",
                br#"{"ComponentName":"a","ComponentVersion":"1.0.0","Manifests":[{"Artifacts":[]}]}"#
                    .to_vec(),
            ),
            ("bytes that are not a document", b"]] not a recipe".to_vec()),
            ("no bytes at all", Vec::new()),
        ];
        for (case, recipe) in rows {
            let found = declared_artifact(&recipe, "first.zip");
            assert_eq!(
                found, None,
                "case {case:?}: expected no artifact, got {found:?}"
            );
        }
    }

    /// RULE: the object an artifact names is addressed path-style, bucket first.
    #[test]
    fn an_objects_path_is_its_bucket_and_key() {
        let object = ArtifactObject {
            bucket: "artifact-bucket".to_string(),
            key: "components/first.zip".to_string(),
        };
        assert_eq!(
            object.path(),
            "/artifact-bucket/components/first.zip",
            "case \"a key with a prefix\": expected the path-style address, got {:?}",
            object.path()
        );
    }
}
