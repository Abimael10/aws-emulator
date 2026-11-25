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
//! What a build environment CANNOT do here is fetch the image a `FROM` instruction names off
//! a registry this emulator does not serve: nothing in this emulator makes an outbound
//! connection. The image the environment itself HOLDS under that reference is another matter —
//! a `docker pull` of one of this emulator's own registries puts it there
//! ([`super::docker_cli`]) — and a build FROM an image the environment holds is built ON it:
//! the layers of the base come first and the context is the layer stacked on them, which is
//! what makes the composed image really derived from the base. A `FROM` naming an image the
//! environment does not hold leaves the composed image its own context as its single layer.
//! The instruction is still required to be there, because a Dockerfile without one is not a
//! Dockerfile a build can run.
//!
//! What the DOCUMENT declares about the image is read beside the context
//! ([`super::dockerfile`]) and composed into the configuration blob: the environment, command,
//! entry point, exposed ports, working directory and user its instructions set, and the
//! `history` whose entries are the instructions as they ran — resolved against the
//! `--build-arg` values the command carried. So an image's digest depends on a build argument
//! exactly where the Dockerfile uses it, as it does on the files of the context.
//!
//! # Bounds
//!
//! Every bound is this emulator's and none is reachable from a command: how many bytes of the
//! working tree one context carries and how many bytes ALL of one image's layers come to
//! ([`MAX_IMAGE_BYTES`]), how many layers one image carries ([`MAX_IMAGE_LAYERS`]), and how
//! many files of the tree one context walks ([`MAX_CONTEXT_FILES`], [`MAX_CONTEXT_DEPTH`]). A
//! build past any of them fails rather than growing this process, which is what bounds a
//! CHAIN of builds each composed on the image before it.

use super::dockerfile;
use ::service_common::posix_shell_session::ProgramFiles;
use ::std::collections::BTreeMap;
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
const FROM_INSTRUCTION: &str = dockerfile::FROM_INSTRUCTION;

/// How many bytes of the build's working tree ONE image carries, and how many bytes ALL the
/// layers of one image come to together.
///
/// CHOSEN, not a Docker or a registry limit: the archive and its compression happen in this
/// process and the blobs are then held in it, so what one image may cost is bounded here. A
/// context past it is a build that fails rather than one that grows this process, and so is an
/// image built on a base whose layers already come to this much — a build stacks a layer on
/// its base, so the bound has to be on the image and not only on the context it adds.
pub const MAX_IMAGE_BYTES: usize = 256 * 1024;

/// How many filesystem layers ONE image this environment holds carries — one per build in the
/// chain that composed it, or one per entry of the manifest a pull delivered.
///
/// CHOSEN for the same reason as [`MAX_IMAGE_BYTES`], and bounding the other factor: a build
/// may be run on the image a build before it composed, so without it the chain a buildspec's
/// commands can make is what decides how many blobs this process holds. A composition past it
/// fails, and so does a pull of a manifest naming more ([`super::registry_client`]).
pub const MAX_IMAGE_LAYERS: usize = 15;

/// How many files of the tree one image's context carries, and how deep the walk goes.
///
/// CHOSEN for the same reason as [`MAX_IMAGE_BYTES`]: the tree is the build's own, so the
/// walk over it is bounded here rather than by the tree.
const MAX_CONTEXT_FILES: usize = 1024;

/// See [`MAX_CONTEXT_FILES`].
const MAX_CONTEXT_DEPTH: usize = 16;

/// One image a build has composed or pulled: the blobs it is made of, which is everything a
/// push sends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalImage {
    /// The image configuration blob, as the bytes the registry stores.
    config: Vec<u8>,
    /// The filesystem layer blobs, each a gzip tar, in the order the manifest lists them:
    /// the base image's layers first, then the one composed from the build's own context.
    layers: Vec<Vec<u8>>,
}

impl LocalImage {
    /// The image a registry HOLDS, as a pull of it delivers: the configuration blob and the
    /// layer blobs its stored manifest names, in that manifest's own order.
    #[must_use]
    pub fn pulled(config: Vec<u8>, layers: Vec<Vec<u8>>) -> Self {
        Self { config, layers }
    }

    /// The configuration blob and its digest.
    #[must_use]
    pub fn config_blob(&self) -> (&[u8], String) {
        (&self.config, content_digest(&self.config))
    }

    /// Every layer blob and its digest, in the order the manifest lists them.
    #[must_use]
    pub fn layer_blobs(&self) -> Vec<(&[u8], String)> {
        self.layers
            .iter()
            .map(|layer| (layer.as_slice(), content_digest(layer)))
            .collect()
    }

    /// The manifest a push sends for this image: the document that names its configuration
    /// and each of its layers, by digest and size.
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
            "layers": self.layers.iter().map(|layer| ::serde_json::json!({
                "mediaType": LAYER_MEDIA_TYPE,
                "size": layer.len(),
                "digest": content_digest(layer),
            })).collect::<Vec<::serde_json::Value>>(),
        })
        .to_string()
    }

    /// What this image's own configuration document states under `member`, for the image built
    /// ON it: a derived image inherits its base's configuration, its filesystem and its
    /// history, and the instructions of the new build add to them.
    ///
    /// `None` for a configuration blob that is no document at all, which states nothing to
    /// inherit.
    fn stated(&self, member: &str) -> Option<::serde_json::Value> {
        let document: ::serde_json::Value = ::serde_json::from_slice(&self.config).ok()?;
        match member.split_once('.') {
            Some((outer, inner)) => document.get(outer)?.get(inner).cloned(),
            None => document.get(member).cloned(),
        }
    }

    /// The entries of the array this image's configuration states under `member`, and none for
    /// a configuration that states no such array.
    fn stated_array(&self, member: &str) -> Vec<::serde_json::Value> {
        self.stated(member)
            .and_then(|stated| stated.as_array().cloned())
            .unwrap_or_default()
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

/// The base a Dockerfile declares, as the document in `files` at `dockerfile` names it
/// resolved against the `--build-arg` values `build_arguments` the command carried.
///
/// It is read here rather than by the caller because a `FROM` is resolved like every other
/// instruction ([`super::dockerfile`]): the reference a build is built ON can come from a
/// build argument. `None` for a document the tree does not hold and one that names no image.
pub fn declared_base(
    files: &dyn ProgramFiles,
    dockerfile: &str,
    build_arguments: &BTreeMap<String, String>,
) -> Option<String> {
    let declaration = files.read(dockerfile)?;
    self::dockerfile::declared(&declaration, build_arguments)
        .base()
        .map(str::to_string)
}

/// Compose the image `docker build` builds from the context at `context` in `files`, built
/// from the Dockerfile at `dockerfile`, with the `--build-arg` values `build_arguments` the
/// command carried, ON `base` — the image the build environment holds under the reference the
/// document's `FROM` names, and `None` when it holds none.
///
/// `Err` carries what the command reports instead: a Dockerfile the tree does not hold, a
/// Dockerfile that is not one, a context with nothing in it, or a context past
/// [`MAX_IMAGE_BYTES`].
pub fn composed(
    files: &dyn ProgramFiles,
    context: &str,
    dockerfile: &str,
    build_arguments: &BTreeMap<String, String>,
    base: Option<&LocalImage>,
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
        carried.push((dockerfile_entry, declaration.clone()));
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
    let declared = self::dockerfile::declared(&declaration, build_arguments);
    let config = configuration(&archive, &declared, base);
    // The image is built ON its base: the base's own layers, then the one this context
    // composes.
    let mut layers: Vec<Vec<u8>> = base.map(|base| base.layers.clone()).unwrap_or_default();
    layers.push(layer);
    // What the IMAGE comes to, which is what this process then holds: a build may be run on
    // the image a build before it composed, so each of the two factors is bounded here rather
    // than only the context this one added.
    if layers.len() > MAX_IMAGE_LAYERS {
        return Err(format!(
            "failed to solve: an image this build environment composes carries at most \
             {MAX_IMAGE_LAYERS} filesystem layers"
        ));
    }
    let held: usize = layers.iter().map(Vec::len).sum();
    if held > MAX_IMAGE_BYTES {
        return Err(format!(
            "no space left on device: an image this build environment composes carries at most \
             {MAX_IMAGE_BYTES} bytes"
        ));
    }
    Ok(LocalImage { config, layers })
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

/// The image configuration document for a layer whose UNCOMPRESSED bytes are `archive`, as
/// the Dockerfile `declared`, for an image built ON `base`.
///
/// `rootfs.diff_ids` names each layer by the digest of its uncompressed archive, which is
/// what an image configuration states about its filesystem, so the base's own entries come
/// first and this context's is the last. An image built on a base INHERITS that base's
/// configuration and its history — the instructions of this build override the members they
/// state and their entries are appended — and it is for the base's platform, because that is
/// the filesystem it carries; an image built on nothing states this environment's own
/// platform, and `config` and `history` are what the document declares
/// ([`super::dockerfile`]).
fn configuration(
    archive: &[u8],
    declared: &self::dockerfile::Declaration,
    base: Option<&LocalImage>,
) -> Vec<u8> {
    let stated = |member: &str| base.and_then(|base| base.stated(member));
    let inherited = |member: &str| base.map(|base| base.stated_array(member)).unwrap_or_default();
    let mut diff_ids = inherited("rootfs.diff_ids");
    diff_ids.push(::serde_json::json!(content_digest(archive)));
    let mut history = inherited("history");
    history.extend(
        declared
            .history()
            .as_array()
            .cloned()
            .unwrap_or_default(),
    );
    let mut config = stated("config")
        .and_then(|stated| stated.as_object().cloned())
        .unwrap_or_default();
    if let Some(declared) = declared.configuration().as_object() {
        config.extend(declared.clone());
    }
    ::serde_json::json!({
        "architecture": stated("architecture")
            .unwrap_or_else(|| ::serde_json::json!(IMAGE_ARCHITECTURE)),
        "os": stated("os").unwrap_or_else(|| ::serde_json::json!(IMAGE_OS)),
        "config": config,
        "rootfs": {
            "type": "layers",
            "diff_ids": diff_ids,
        },
        "history": history,
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
            let image = composed(&tree, ".", DEFAULT_DOCKERFILE, &BTreeMap::new(), None)
                .unwrap_or_else(|error| panic!("case {case:?}: expected an image, got {error}"));
            let digest = content_digest(image.manifest().as_bytes());
            // The same context, composed again, is the same image.
            let again = composed(&tree, ".", DEFAULT_DOCKERFILE, &BTreeMap::new(), None)
                .expect("the same image");
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
                Some(image.layer_blobs()[0].1.as_str()),
                "case {case:?}: the manifest must name the layer blob's own digest, got \
                 {manifest}"
            );
            assert_eq!(
                manifest["layers"][0]["size"].as_u64(),
                u64::try_from(image.layer_blobs()[0].0.len()).ok(),
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
            let answered = composed(&tree, ".", DEFAULT_DOCKERFILE, &BTreeMap::new(), None);
            assert_eq!(
                answered.is_ok(),
                composes,
                "case {case:?}: expected composed={composes}, got {:?}",
                answered.map(|image| image.config_blob().1)
            );
        }
    }

    /// RULE: an image built ON a base is built on that base's own layers — the base's layers
    /// come first, the context composes the one stacked on them, and the configuration states
    /// the filesystem and the history of both.
    ///
    /// The rows differ along what the rule generalises over: how many layers the base carries
    /// and whether there is a base at all.
    #[test]
    fn an_image_built_on_a_base_carries_that_bases_layers() {
        let tree = Tree::of(&[("Dockerfile", "FROM base:one"), ("app.py", "print(1)")]);
        let own = composed(&tree, ".", DEFAULT_DOCKERFILE, &BTreeMap::new(), None)
            .expect("an image built on nothing");
        // (case, the base, how many layers the composed image carries)
        let rows: Vec<(&str, Option<LocalImage>, usize)> = vec![
            ("no base the environment holds", None, 1),
            ("a base of one layer", Some(own.clone()), 2),
            (
                "a base of two layers",
                Some(
                    composed(&tree, ".", DEFAULT_DOCKERFILE, &BTreeMap::new(), Some(&own))
                        .expect("an image built on one"),
                ),
                3,
            ),
        ];
        for (case, base, layers) in rows {
            let image = composed(
                &tree,
                ".",
                DEFAULT_DOCKERFILE,
                &BTreeMap::new(),
                base.as_ref(),
            )
            .unwrap_or_else(|error| panic!("case {case:?}: expected an image, got {error}"));
            let blobs = image.layer_blobs();
            assert_eq!(
                blobs.len(),
                layers,
                "case {case:?}: expected {layers} layers, got {}",
                blobs.len()
            );
            // The base's own layers come first, in their own order.
            if let Some(base) = base.as_ref() {
                let under: Vec<String> = base
                    .layer_blobs()
                    .iter()
                    .map(|(_blob, digest)| digest.clone())
                    .collect();
                let stacked: Vec<String> = blobs
                    .iter()
                    .take(under.len())
                    .map(|(_blob, digest)| digest.clone())
                    .collect();
                assert_eq!(
                    stacked, under,
                    "case {case:?}: expected the base's layers beneath, got {stacked:?}"
                );
            }
            // The manifest names every layer, and the configuration states one filesystem
            // entry per layer.
            let manifest: ::serde_json::Value =
                ::serde_json::from_str(&image.manifest()).expect("a manifest document");
            assert_eq!(
                manifest["layers"].as_array().map(Vec::len),
                Some(layers),
                "case {case:?}: the manifest must name every layer, got {manifest}"
            );
            let configuration: ::serde_json::Value =
                ::serde_json::from_slice(image.config_blob().0).expect("a configuration");
            assert_eq!(
                configuration["rootfs"]["diff_ids"].as_array().map(Vec::len),
                Some(layers),
                "case {case:?}: the configuration must state every layer's filesystem, got \
                 {configuration}"
            );
        }
    }

    /// RULE: an image this environment holds is bounded — in the layers it carries and in what
    /// they come to — so a CHAIN of builds each composed on the image before it is refused
    /// rather than growing this process without end.
    #[test]
    fn an_image_is_bounded_however_long_the_chain_that_composed_it() {
        let tree = Tree::of(&[("Dockerfile", "FROM base:one"), ("app.py", "print(1)")]);
        let mut held = composed(&tree, ".", DEFAULT_DOCKERFILE, &BTreeMap::new(), None)
            .expect("an image built on nothing");
        // Each composition stacks one more layer on the image before it.
        for step in 2..=MAX_IMAGE_LAYERS {
            held = composed(
                &tree,
                ".",
                DEFAULT_DOCKERFILE,
                &BTreeMap::new(),
                Some(&held),
            )
            .unwrap_or_else(|error| panic!("step {step}: expected an image, got {error}"));
            assert_eq!(
                held.layer_blobs().len(),
                step,
                "step {step}: expected one layer per composition, got {}",
                held.layer_blobs().len()
            );
        }
        let past = composed(
            &tree,
            ".",
            DEFAULT_DOCKERFILE,
            &BTreeMap::new(),
            Some(&held),
        );
        assert!(
            past.is_err(),
            "a composition past the bound is refused, got {:?} layers",
            past.map(|image| image.layer_blobs().len())
        );
    }
}
