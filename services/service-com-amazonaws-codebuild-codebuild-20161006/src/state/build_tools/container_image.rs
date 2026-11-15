// SPDX-License-Identifier: CC0-1.0

//! The IMAGE a `docker build` composes out of a build's own working tree.
//!
//! # The rule this module implements
//!
//! "The `docker build` command builds an image from a Dockerfile and a context", where the
//! context is "the set of files located in the specified PATH" and the Dockerfile has to be
//! there to build from at all (Docker CLI reference). An image is then addressed by CONTENT:
//! its manifest names the configuration blob and each layer blob by the SHA-256 digest of
//! that blob's bytes, and the image's own digest is the digest of the manifest — which is
//! what makes one build of one context the same image wherever it is pushed, and a build of
//! different files a different image.
//!
//! So the image this module composes is composed from those files and from nothing else: the
//! layer is a real gzip-compressed tar archive of the context's files, the configuration is
//! the image configuration document that names the layer's uncompressed digest, and all four
//! digests are computed over those bytes. Nothing is stamped with the clock, so the same
//! context composes the same image — a digest is a fact about content, not about when it was
//! built.
//!
//! The media types are the ones a Docker image manifest carries
//! ([`MANIFEST_MEDIA_TYPE`], [`CONFIG_MEDIA_TYPE`], [`LAYER_MEDIA_TYPE`]: OCI's "Docker
//! Image Manifest V2, Schema 2" set, which is what `docker push` sends and what Amazon ECR
//! reports as a pushed image's `imageManifestMediaType`), so the blobs and the manifest this
//! composes are the ones a registry client would have sent, and the image a registry holds
//! after the push is one a client can read back.
//!
//! What a build environment CANNOT do here is fetch the image a `FROM` instruction names:
//! nothing in this emulator reaches a registry off it, so the image a build composes carries
//! its own context as its single layer and no base layer. The instruction is still required
//! to be there, because a Dockerfile without one is not a Dockerfile a build can run.

use ::service_common::posix_shell_session::ProgramFiles;
use ::std::io::Write as _;

/// The media type of the manifest this composes (Docker Image Manifest V2, Schema 2).
pub const MANIFEST_MEDIA_TYPE: &str = "application/vnd.docker.distribution.manifest.v2+json";

/// The media type of an image's configuration blob.
pub const CONFIG_MEDIA_TYPE: &str = "application/vnd.docker.container.image.v1+json";

/// The media type of a gzip-compressed layer blob.
pub const LAYER_MEDIA_TYPE: &str = "application/vnd.docker.image.rootfs.diff.tar.gzip";

/// The file a build is built FROM, under the context path, as `docker build` looks for it
/// when the command names none.
pub const DEFAULT_DOCKERFILE: &str = "Dockerfile";

/// The instruction a Dockerfile has to open with: "a Dockerfile must begin with a `FROM`
/// instruction" (Dockerfile reference), ignoring the parser directives and comments before
/// it.
const FROM_INSTRUCTION: &str = "FROM";

/// How many bytes of the build's working tree ONE image carries.
///
/// CHOSEN, not a Docker or a registry limit: the archive and its compression happen in this
/// process and the blobs are then held in it, so what one image may cost is bounded here. A
/// context past it is a build that fails rather than one that grows this process.
pub const MAX_IMAGE_BYTES: usize = 256 * 1024;

/// How many files of the tree one image's context carries, and how deep the walk goes.
///
/// CHOSEN for the same reason as [`MAX_IMAGE_BYTES`]: the tree is the build's own, so the
/// walk over it is bounded here rather than by the tree.
const MAX_CONTEXT_FILES: usize = 1024;

/// See [`MAX_CONTEXT_FILES`].
const MAX_CONTEXT_DEPTH: usize = 16;

/// One image a build has composed: the blobs it is made of, which is everything a push
/// sends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalImage {
    /// The image configuration blob, as the bytes the registry stores.
    config: Vec<u8>,
    /// The one layer blob: the gzip tar of the context the image was built from.
    layer: Vec<u8>,
}

impl LocalImage {
    /// The configuration blob and its digest.
    #[must_use]
    pub fn config_blob(&self) -> (&[u8], String) {
        (&self.config, content_digest(&self.config))
    }

    /// The layer blob and its digest.
    #[must_use]
    pub fn layer_blob(&self) -> (&[u8], String) {
        (&self.layer, content_digest(&self.layer))
    }

    /// The manifest a push sends for this image: the document that names its configuration
    /// and its layer, each by digest and size.
    #[must_use]
    pub fn manifest(&self) -> String {
        ::serde_json::json!({
            "schemaVersion": 2,
            "mediaType": MANIFEST_MEDIA_TYPE,
            "config": {
                "mediaType": CONFIG_MEDIA_TYPE,
                "size": self.config.len(),
                "digest": content_digest(&self.config),
            },
            "layers": [{
                "mediaType": LAYER_MEDIA_TYPE,
                "size": self.layer.len(),
                "digest": content_digest(&self.layer),
            }],
        })
        .to_string()
    }
}

/// The `sha256:<hex>` digest of `bytes`, which is how a registry addresses a blob and a
/// manifest.
#[must_use]
pub fn content_digest(bytes: &[u8]) -> String {
    use ::sha2::Digest as _;
    let mut hasher = ::sha2::Sha256::new();
    hasher.update(bytes);
    format!("sha256:{:x}", hasher.finalize())
}

/// Compose the image `docker build` builds from the context at `context` in `files`, built
/// from the Dockerfile at `dockerfile`.
///
/// `Err` carries what the command reports instead: a Dockerfile the tree does not hold, a
/// Dockerfile that is not one, a context with nothing in it, or a context past
/// [`MAX_IMAGE_BYTES`].
pub fn composed(
    files: &dyn ProgramFiles,
    context: &str,
    dockerfile: &str,
) -> Result<LocalImage, String> {
    let Some(declaration) = files.read(dockerfile) else {
        return Err(format!(
            "failed to solve: failed to read dockerfile: open {dockerfile}: no such file or \
             directory"
        ));
    };
    if !declares_a_base(&declaration) {
        return Err(format!(
            "failed to solve: dockerfile parse error: file with no instructions: {dockerfile}"
        ));
    }
    let mut carried = tree_under(files, context);
    // The Dockerfile is part of what a build sends, whether or not it is under the context
    // path (`-f` may name one outside it).
    let dockerfile_entry = entry_name(context, dockerfile);
    if !carried.iter().any(|(path, _)| path == &dockerfile_entry) {
        carried.push((dockerfile_entry, declaration));
    }
    if carried.is_empty() {
        return Err(format!(
            "error checking context: can't stat '{context}': no such file or directory"
        ));
    }
    let carried_bytes: usize = carried
        .iter()
        .map(|(path, contents)| path.len().saturating_add(contents.len()))
        .sum();
    if carried_bytes > MAX_IMAGE_BYTES {
        return Err(format!(
            "error checking context: the context carries more than the {MAX_IMAGE_BYTES} bytes \
             one image is built from"
        ));
    }
    let archive = archived(&carried)?;
    let layer = compressed(&archive)?;
    let config = configuration(&archive);
    Ok(LocalImage { config, layer })
}

/// Whether `declaration` is a Dockerfile at all: one that states the `FROM` instruction a
/// Dockerfile has to begin with, past its comments and parser directives.
fn declares_a_base(declaration: &str) -> bool {
    declaration
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('#'))
        .is_some_and(|line| {
            line.split_whitespace()
                .next()
                .is_some_and(|word| word.eq_ignore_ascii_case(FROM_INSTRUCTION))
        })
}

/// The files of the tree under `directory` — the context a build sends — as
/// `(entry name, contents)` in path order, where the entry name is the path relative to the
/// context root, which is the name the archive carries.
fn tree_under(files: &dyn ProgramFiles, directory: &str) -> Vec<(String, String)> {
    let root = normalised(directory);
    let mut carried: Vec<(String, String)> = Vec::new();
    let mut pending: Vec<(String, usize)> = vec![(root.clone(), 0)];
    while let Some((path, depth)) = pending.pop() {
        if carried.len() >= MAX_CONTEXT_FILES || depth > MAX_CONTEXT_DEPTH {
            break;
        }
        for name in files.entries_in(&path) {
            let child = match path.as_str() {
                "" => name.clone(),
                held => format!("{held}/{name}"),
            };
            match files.read(&child) {
                Some(contents) => carried.push((entry_name(directory, &child), contents)),
                // Nothing to read there: a directory of the tree, whose own entries are
                // what the context carries.
                None => pending.push((child, depth + 1)),
            }
        }
    }
    carried.sort();
    carried.dedup();
    carried
}

/// `path` with the leading `./` and the trailing `/` a command may write it with removed,
/// and the empty string for the tree's own root — the form the tree keys its files by.
fn normalised(path: &str) -> String {
    let held = path.trim();
    let held = held.strip_prefix("./").unwrap_or(held);
    match held {
        "" | "." | "/" => String::new(),
        named => named.trim_end_matches('/').to_string(),
    }
}

/// The name the archive carries `path` under: its path relative to the context root.
fn entry_name(context: &str, path: &str) -> String {
    let root = normalised(context);
    let path = normalised(path);
    if root.is_empty() {
        return path;
    }
    path.strip_prefix(&format!("{root}/"))
        .unwrap_or(&path)
        .to_string()
}

/// The tar archive of `carried`: the layer's own uncompressed bytes.
fn archived(carried: &[(String, String)]) -> Result<Vec<u8>, String> {
    let mut builder = ::tar::Builder::new(Vec::new());
    for (path, contents) in carried {
        let mut header = ::tar::Header::new_gnu();
        header.set_size(u64::try_from(contents.len()).unwrap_or_default());
        header.set_mode(0o644);
        // Not stamped with the clock: an image is addressed by content, so the same context
        // has to compose the same digest.
        header.set_mtime(0);
        header.set_cksum();
        builder
            .append_data(&mut header, path, contents.as_bytes())
            .map_err(|error| format!("failed to solve: failed to archive {path}: {error}"))?;
    }
    builder
        .into_inner()
        .map_err(|error| format!("failed to solve: failed to archive the context: {error}"))
}

/// `archive`, gzip-compressed, which is what the layer's media type says the blob is.
fn compressed(archive: &[u8]) -> Result<Vec<u8>, String> {
    let mut encoder =
        ::flate2::write::GzEncoder::new(Vec::new(), ::flate2::Compression::default());
    encoder
        .write_all(archive)
        .and_then(|()| encoder.finish())
        .map_err(|error| format!("failed to solve: failed to compress the layer: {error}"))
}

/// The architecture an image this environment composes states.
///
/// The build environment runs in this process, so the image it composes is for the platform
/// this process is built for, as the image configuration spells those values.
const IMAGE_ARCHITECTURE: &str = ::std::env::consts::ARCH;

/// See [`IMAGE_ARCHITECTURE`].
const IMAGE_OS: &str = ::std::env::consts::OS;

/// The image configuration document for a layer whose UNCOMPRESSED bytes are `archive`.
///
/// `rootfs.diff_ids` names each layer by the digest of its uncompressed archive, which is
/// what an image configuration states about its filesystem; the platform values are this
/// environment's own.
fn configuration(archive: &[u8]) -> Vec<u8> {
    ::serde_json::json!({
        "architecture": IMAGE_ARCHITECTURE,
        "os": IMAGE_OS,
        "config": {},
        "rootfs": {
            "type": "layers",
            "diff_ids": [content_digest(archive)],
        },
    })
    .to_string()
    .into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::std::collections::BTreeMap;

    /// One row of the second table below: the case, the files the tree holds, and whether an
    /// image is composed from them.
    type ComposedRow = (&'static str, Vec<(&'static str, &'static str)>, bool);

    /// A tree of files, as the session hands one to a program.
    #[derive(Debug, Default)]
    struct Tree {
        files: BTreeMap<String, String>,
    }

    impl Tree {
        fn of(files: &[(&str, &str)]) -> Self {
            Self {
                files: files
                    .iter()
                    .map(|(path, contents)| ((*path).to_string(), (*contents).to_string()))
                    .collect(),
            }
        }
    }

    impl ProgramFiles for Tree {
        fn read(&self, path: &str) -> Option<String> {
            self.files.get(path).cloned()
        }

        fn write(&mut self, path: &str, contents: &str) -> bool {
            self.files.insert(path.to_string(), contents.to_string());
            true
        }

        fn remove(&mut self, path: &str) -> bool {
            self.files.remove(path).is_some()
        }

        fn entries_in(&self, path: &str) -> Vec<String> {
            let prefix = if path.is_empty() {
                String::new()
            } else {
                format!("{path}/")
            };
            let mut names: Vec<String> = Vec::new();
            for held in self.files.keys() {
                let Some(relative) = held.strip_prefix(prefix.as_str()) else {
                    continue;
                };
                let name = relative.split('/').next().unwrap_or(relative).to_string();
                if !name.is_empty() && !names.contains(&name) {
                    names.push(name);
                }
            }
            names
        }
    }

    /// RULE: an image is addressed by the CONTENT it was built from — the same context
    /// composes the same digests, and a context differing in any file composes different
    /// ones.
    ///
    /// The rows differ along what the rule generalises over: which files the context holds,
    /// how deep they are, and what they contain.
    #[test]
    fn an_image_is_addressed_by_the_content_it_was_built_from() {
        // (case, the files the tree holds)
        let rows: Vec<(&str, Vec<(&str, &str)>)> = vec![
            ("the Dockerfile alone", vec![("Dockerfile", "FROM scratch")]),
            (
                "a file beside it",
                vec![("Dockerfile", "FROM scratch"), ("app.py", "print(1)")],
            ),
            (
                "the same files with one changed",
                vec![("Dockerfile", "FROM scratch"), ("app.py", "print(2)")],
            ),
            (
                "a nested directory",
                vec![
                    ("Dockerfile", "FROM scratch"),
                    ("src/app.py", "print(1)"),
                    ("src/lib/util.py", "pass"),
                ],
            ),
        ];
        let mut seen: BTreeMap<String, &str> = BTreeMap::new();
        for (case, files) in rows {
            let tree = Tree::of(&files);
            let image = composed(&tree, ".", DEFAULT_DOCKERFILE)
                .unwrap_or_else(|error| panic!("case {case:?}: expected an image, got {error}"));
            let digest = content_digest(image.manifest().as_bytes());
            // The same context, composed again, is the same image.
            let again = composed(&tree, ".", DEFAULT_DOCKERFILE).expect("the same image");
            assert_eq!(
                image, again,
                "case {case:?}: the same context must compose the same image, got {:?} then {:?}",
                image.config_blob().1,
                again.config_blob().1
            );
            // A different context is a different image.
            if let Some(other) = seen.insert(digest.clone(), case) {
                panic!("case {case:?}: expected a digest of its own, got {digest} from {other:?}");
            }
            // The manifest names both blobs by their own digests and sizes.
            let manifest: ::serde_json::Value =
                ::serde_json::from_str(&image.manifest()).expect("a manifest document");
            assert_eq!(
                manifest["config"]["digest"].as_str(),
                Some(image.config_blob().1.as_str()),
                "case {case:?}: the manifest must name the configuration blob's own digest, \
                 got {manifest}"
            );
            assert_eq!(
                manifest["layers"][0]["digest"].as_str(),
                Some(image.layer_blob().1.as_str()),
                "case {case:?}: the manifest must name the layer blob's own digest, got \
                 {manifest}"
            );
            assert_eq!(
                manifest["layers"][0]["size"].as_u64(),
                u64::try_from(image.layer_blob().0.len()).ok(),
                "case {case:?}: the manifest must state the layer's own size, got {manifest}"
            );
        }
    }

    /// RULE: a build needs a Dockerfile to build from, and a file that states no instruction
    /// is not one.
    #[test]
    fn a_build_needs_a_dockerfile_to_build_from() {
        // (case, the files, whether an image is composed)
        let rows: Vec<ComposedRow> = vec![
            (
                "a Dockerfile that declares a base",
                vec![("Dockerfile", "FROM scratch\nCOPY . /app")],
                true,
            ),
            (
                "a Dockerfile behind comments",
                vec![("Dockerfile", "# syntax=docker/dockerfile:1\n\nFROM scratch")],
                true,
            ),
            ("no Dockerfile at all", vec![("app.py", "print(1)")], false),
            (
                "a file that states no instruction",
                vec![("Dockerfile", "# nothing here\n")],
                false,
            ),
            (
                "a file whose first instruction is not FROM",
                vec![("Dockerfile", "RUN true\nFROM scratch")],
                false,
            ),
        ];
        for (case, files, composes) in rows {
            let tree = Tree::of(&files);
            let answered = composed(&tree, ".", DEFAULT_DOCKERFILE);
            assert_eq!(
                answered.is_ok(),
                composes,
                "case {case:?}: expected composed={composes}, got {:?}",
                answered.map(|image| image.config_blob().1)
            );
        }
    }
}
