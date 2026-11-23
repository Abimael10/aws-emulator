// SPDX-License-Identifier: CC0-1.0

//! What the served Kubernetes API answers `/version` with: the version of the
//! BUILD running the cluster's API server.
//!
//! A Kubernetes client reads `/version` before it decides anything else — whether
//! a resource it wants exists in this release, whether its own version skew is
//! within the supported window, which API group version to address — and it reads
//! that decision out of ONE member: `gitVersion`, "the semantic version of the
//! binary", which Kubernetes clients parse as a semantic version and therefore as
//! `major.minor.patch` with a build suffix.
//!
//! On Amazon EKS that build is EKS's own build of the Kubernetes release, so the
//! string is of the form `v<major>.<minor>.<patch>-eks-<build>`: the leading `v`,
//! the dots and the `-eks-` marker are fixed, the major and minor are the version
//! of the CLUSTER (the one `DescribeCluster` reports, so the two answers about one
//! cluster cannot disagree), and the patch and the build id come from the control
//! plane. This service's control plane publishes one patch level per Kubernetes
//! version ([`super::ClusterVersionRecord::kubernetes_patch_version`], the same
//! value `DescribeClusterVersions` reports) and one build per patch level, derived
//! from the patch version so that a cluster of a version always reports the same
//! build — the way every other derived identifier of this service is derived
//! ([`super::derived_identifiers`]) rather than minted per call.
//!
//! The members beside `gitVersion` describe the same build: `major` and `minor`
//! are its version's two components, `gitCommit`, `gitTreeState` and `buildDate`
//! are the build's identity — the commit the build is of, whether its tree carried
//! uncommitted changes, and when it was built, which for a published Kubernetes
//! version is the day Amazon EKS released that version — and `platform` is the
//! operating system and architecture the server ANSWERING the request runs on,
//! read off this process rather than stated.
//!
//! The Go toolchain members a Kubernetes binary reports (`goVersion`, `compiler`)
//! are NOT reported: this API server is not a Go build, and a Go release and
//! compiler name for a binary no Go toolchain produced would be a value nothing
//! computed. Every member the document does carry is one about this build.

use ::std::time::SystemTime;

use super::kubernetes_objects::rfc3339_seconds;

/// How many hexadecimal digits the `-eks-` build id of a `gitVersion` carries.
///
/// An EKS build id is the short form of the commit the build is of — the same
/// short form Kubernetes' own `gitVersion` build metadata carries — which is seven
/// digits.
const BUILD_ID_DIGITS: usize = 7;

/// How many hexadecimal digits a `gitCommit` carries: a full Git object name.
const GIT_COMMIT_DIGITS: usize = 40;

/// The `gitTreeState` of a published build: Kubernetes reports `clean` for a build
/// made from a tree with no uncommitted change, which is what a released build is.
const CLEAN_TREE: &str = "clean";

/// The Kubernetes version document of the build serving a cluster of
/// `kubernetes_version`, whose control plane publishes `patch_version` as that
/// version's patch level and released it at `released_at`.
///
/// `released_at` is `None` for a cluster whose version the published calendar does
/// not carry, which reports no build date rather than one invented for it.
#[must_use]
pub fn version_document(
    kubernetes_version: &str,
    patch_version: &str,
    released_at: Option<SystemTime>,
) -> ::serde_json::Value {
    let mut parts = kubernetes_version.split('.');
    let major = parts.next().unwrap_or_default();
    let minor = parts.next().unwrap_or_default();
    let commit = build_commit(patch_version);
    let mut document = ::serde_json::json!({
        "major": major,
        "minor": minor,
        "gitVersion": git_version(patch_version),
        "gitCommit": commit,
        "gitTreeState": CLEAN_TREE,
        "platform": serving_platform(),
    });
    if let Some(released_at) = released_at {
        document["buildDate"] = ::serde_json::json!(rfc3339_seconds(released_at));
    }
    document
}

/// The `gitVersion` of the EKS build of `patch_version`: the patch-level version
/// with the `-eks-<build>` suffix every Amazon EKS build of Kubernetes carries.
#[must_use]
pub fn git_version(patch_version: &str) -> String {
    let build = build_commit(patch_version);
    let short: String = build.chars().take(BUILD_ID_DIGITS).collect();
    format!("v{patch_version}-eks-{short}")
}

/// The commit the build of `patch_version` is of, as a Git object name.
///
/// DERIVED from the patch version and nothing else, through the one stable mixer of
/// this workspace (`service_common::stable_hex_id`, sixteen hexadecimal digits per
/// call), so a cluster of a Kubernetes version reports the same build on every read
/// and in every build of this emulator, and two Kubernetes versions report
/// different ones. It is an opaque id: nothing authenticates on it.
fn build_commit(patch_version: &str) -> String {
    let mut digits = String::new();
    // One call answers sixteen digits, so the object name is mixed in labelled
    // chunks until it is long enough, and cut to the length a Git object name has.
    let mut chunk = 0u32;
    while digits.len() < GIT_COMMIT_DIGITS {
        digits.push_str(&::service_common::stable_hex_id(&[
            "kubernetes-build",
            patch_version,
            &chunk.to_string(),
        ]));
        chunk += 1;
    }
    digits.truncate(GIT_COMMIT_DIGITS);
    digits
}

/// The `platform` this API server answers on: the operating system and the
/// architecture of the process serving the request, in the `<os>/<arch>` form
/// Kubernetes writes it in (`linux/amd64`).
///
/// Read off this build's own target rather than stated, and translated into the
/// names Go — and therefore every Kubernetes client parsing this member — spells
/// them with, which is what the pair a client reads has to be to mean anything to
/// it.
#[must_use]
pub fn serving_platform() -> String {
    format!("{}/{}", ::std::env::consts::OS, go_architecture())
}

/// The name Go gives this build's target architecture: `amd64` and `arm64` are the
/// two names a Kubernetes platform pair carries for the architectures this emulator
/// is built for, and any other target is reported under the name its own toolchain
/// gives it.
fn go_architecture() -> &'static str {
    match ::std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        other => other,
    }
}
