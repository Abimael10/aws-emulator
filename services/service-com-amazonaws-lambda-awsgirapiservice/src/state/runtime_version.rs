// SPDX-License-Identifier: CC0-1.0

//! The managed runtime VERSION a function runs on: the ARN it is addressed by, the digest
//! that ARN carries, the BUILD an execution environment reports initialising on, and the
//! runtime an ARN names back.
//!
//! A runtime version is a BUILD of one managed runtime — "each runtime version is associated
//! with a version number and an ARN" — so every reading of it here is derived from the
//! runtime identifier the function is configured with and from nothing else: the ARN, the
//! digest it carries and the build the environment's own record names are three statements of
//! one version, and two reads of a function on one runtime report the same version while two
//! runtimes report different ones. Nothing is taken from the clock or from a request.
//!
//! Because the version is derived, it can also be RESOLVED the other way
//! ([`runtime_named_by_version_arn`]): the runtime an offered `RuntimeVersionArn` is a build
//! of is what decides whether a function may be pinned to it at all, which is the one
//! compatibility judgement `PutRuntimeManagementConfig` makes.

use sha2::Digest as _;

/// The ARN of the managed runtime version a function is pinned to:
/// `arn:aws:lambda:<region>::runtime:<digest>`, the first form the model's
/// `RuntimeVersionArn` pattern admits.
///
/// The resource position of that form holds the SHA-256 DIGEST of the runtime build the
/// version is, which AWS reports in full: 64 lowercase hexadecimal digits, the width a
/// client that pins or compares a runtime version (runtime-management configuration, drift
/// detection) reads it at. So it is the whole digest rather than a prefix of one, and it is
/// DERIVED from the runtime identifier.
#[must_use]
pub fn runtime_version_arn(region: &str, runtime: &str) -> String {
    format!(
        "arn:aws:lambda:{region}::runtime:{}",
        runtime_version_digest(runtime)
    )
}

/// The 64 lowercase hexadecimal digits of the runtime build `runtime` names: the SHA-256
/// of the identifier, rendered whole.
///
/// The digest comes from the maintained `sha2` crate and its hexadecimal from `hex`, so
/// neither is written out here; it is the one place the width of a runtime-version digest
/// is decided.
#[must_use]
pub fn runtime_version_digest(runtime: &str) -> String {
    ::hex::encode(sha2::Sha256::digest(runtime.as_bytes()))
}

/// How the resource of the SECOND form the `RuntimeVersionArn` pattern admits opens:
/// `arn:<partition>:lambda:<region>:<account>:runtime/<name>/version/<version>`, which names
/// the runtime it is a version OF in the path itself.
const RUNTIME_PATH_PREFIX: &str = ":runtime/";

/// What separates that path's runtime name from the version under it.
const RUNTIME_PATH_VERSION_INFIX: &str = "/version/";

/// How the resource of the FIRST form opens: `arn:<partition>:lambda:<region>::runtime:<id>`,
/// whose id is the digest above.
const RUNTIME_DIGEST_PREFIX: &str = ":runtime:";

/// The runtime identifier the runtime version `arn` is a build OF, or `None` for an ARN this
/// account holds no runtime version under.
///
/// Both forms the model's `RuntimeVersionArn` pattern admits are read: the path form names
/// the runtime in the path, and the digest form is resolved by the digest every managed
/// runtime's version ARN carries ([`runtime_version_digest`]), over the runtimes the model's
/// own `Runtime` enum declares. A runtime version this emulator mints for no runtime at all
/// names none, which is all that can be said about it.
#[must_use]
pub fn runtime_named_by_version_arn(arn: &str) -> Option<String> {
    if let Some((_, path)) = arn.split_once(RUNTIME_PATH_PREFIX) {
        let (name, _version) = path.split_once(RUNTIME_PATH_VERSION_INFIX)?;
        return (!name.is_empty()).then(|| name.to_string());
    }
    let (_, digest) = arn.split_once(RUNTIME_DIGEST_PREFIX)?;
    crate::ops::RUNTIME_VALUES
        .iter()
        .find(|runtime| runtime_version_digest(runtime) == digest)
        .map(|runtime| (*runtime).to_string())
}

/// What a runtime version's build NUMBER is written after, as AWS publishes one: a version of
/// the `python3.12` runtime is `python:3.12.v<build>`.
const BUILD_PREFIX: &str = "v";

/// How many builds a version number is one of. The published numbers are a short count of
/// builds of one runtime line, so a build derived into this range is one of the shape a client
/// parses; which one it is follows from the version's own digest below.
const BUILD_NUMBERS: u32 = 99;

/// How many hexadecimal digits of the digest the build number is derived from.
const BUILD_DIGITS: usize = 4;

/// The runtime BUILD the version of `runtime` is, in the form AWS names a runtime version by —
/// `<family>:<line>.v<build>`, so `python3.12` is a `python:3.12.v<build>` build and
/// `nodejs22.x` a `nodejs:22.v<build>` one.
///
/// It is the reading an execution environment's own initialization record states
/// ([`super::execution`]'s `INIT_START Runtime Version:`), beside the ARN of the same version:
/// the identifier names the runtime, while the build names which build of it the environment
/// stands on, which is what a client auditing a runtime-version rollout reads. The build is
/// derived from the same digest the version's ARN carries, so the two statements name ONE
/// version of ONE runtime.
///
/// `None` for an identifier that states no version line at all (`provided`, `nodejs`): there
/// is no `<family>:<line>` to name a build of, so the identifier itself is all there is to
/// report.
#[must_use]
pub fn runtime_build(runtime: &str) -> Option<String> {
    let (family, line) = family_and_line(runtime)?;
    Some(format!(
        "{family}:{line}.{BUILD_PREFIX}{}",
        build_number(runtime)
    ))
}

/// The FAMILY `runtime` names and the version LINE of it the identifier states: the leading
/// letters are the family (`python`, `nodejs`, `provided`), and what follows is the line, with
/// the separator the identifier writes before it and the wildcard a line of a single-component
/// version carries (`nodejs22.x` is the 22 line) left off.
fn family_and_line(runtime: &str) -> Option<(&str, &str)> {
    let at = runtime.find(|character: char| !character.is_ascii_alphabetic())?;
    let (family, stated) = runtime.split_at(at);
    let line = stated
        .trim_start_matches('.')
        .trim_end_matches(LINE_WILDCARD_SUFFIX);
    (!family.is_empty() && !line.is_empty()).then_some((family, line))
}

/// What an identifier whose version line is a single component writes after it (`nodejs22.x`,
/// `go1.x`), and which the line itself does not carry.
const LINE_WILDCARD_SUFFIX: &str = ".x";

/// WHICH build of its line a runtime version is, derived from the digest its ARN carries so
/// the build and the ARN are two statements of one version: stable for one runtime, different
/// between two, and taken from neither the clock nor a request.
fn build_number(runtime: &str) -> u32 {
    let digest = runtime_version_digest(runtime);
    let leading = digest
        .get(..BUILD_DIGITS)
        .and_then(|digits| u32::from_str_radix(digits, 16).ok())
        .unwrap_or_default();
    leading % BUILD_NUMBERS + 1
}

/// The refusal a `RuntimeVersionArn` that is a build of ANOTHER runtime than the function's
/// own gets: the version could not run the function, so the pin names no configuration the
/// function can be in.
///
/// The sentence names both halves of the comparison — the ARN the request offered and the
/// runtime the function is configured with — so a caller reads which pin was refused and
/// against what.
#[must_use]
pub fn incompatible_runtime_version_message(runtime_version_arn: &str, runtime: &str) -> String {
    format!(
        "Runtime version ARN ({runtime_version_arn}) is not compatible with your function's \
         runtime ({runtime})."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A runtime version ARN this emulator mints for a runtime resolves BACK to that runtime,
    /// in both forms the model's `RuntimeVersionArn` pattern admits, and an ARN that is no
    /// runtime's version resolves to none.
    ///
    /// Source of the identifiers: the model's own `Runtime` enum
    /// (`smithy_query(query="shape", shape="Runtime")`, whose values are
    /// `crate::ops::RUNTIME_VALUES`).
    #[test]
    fn a_runtime_version_arn_names_the_runtime_it_is_a_build_of() {
        for runtime in crate::ops::RUNTIME_VALUES {
            let arn = runtime_version_arn("us-east-1", runtime);
            let actual = runtime_named_by_version_arn(&arn);
            assert_eq!(
                actual.as_deref(),
                Some(*runtime),
                "case {runtime:?}: expected the digest form {arn:?} to name {runtime:?}, got \
                 {actual:?}"
            );
            let path = format!(
                "arn:aws:lambda:us-east-1:000000000000:runtime/{runtime}/version/a1b2c3d4e5"
            );
            let actual = runtime_named_by_version_arn(&path);
            assert_eq!(
                actual.as_deref(),
                Some(*runtime),
                "case {runtime:?}: expected the path form {path:?} to name {runtime:?}, got \
                 {actual:?}"
            );
        }
        // An ARN of neither form, and one whose digest is no runtime's, name no runtime at
        // all: nothing can be said about which runtime they are a build of.
        let unknown: &[&str] = &[
            "arn:aws:lambda:us-east-1::runtime:8eeff65f6809a3ce81507fe733fe09b835899b99481ba22fd\
             75b5a7338290ec1",
            "arn:aws:lambda:us-east-1:000000000000:function:not-a-runtime-version",
            "",
        ];
        for arn in unknown {
            let actual = runtime_named_by_version_arn(arn);
            assert_eq!(
                actual, None,
                "case {arn:?}: expected no runtime, got {actual:?}"
            );
        }
    }

    /// The BUILD a runtime version is reported as names the family and the version line the
    /// runtime identifier states, and a build of that line.
    #[test]
    fn a_runtime_build_names_the_family_and_the_line_the_identifier_states() {
        // (the identifier, the `<family>:<line>` the build opens with)
        let cases: &[(&str, Option<&str>)] = &[
            ("python3.12", Some("python:3.12.")),
            ("python3.9", Some("python:3.9.")),
            ("python2.7", Some("python:2.7.")),
            ("nodejs22.x", Some("nodejs:22.")),
            ("nodejs18.x", Some("nodejs:18.")),
            ("nodejs4.3-edge", Some("nodejs:4.3-edge.")),
            ("java21", Some("java:21.")),
            ("java11.al2023", Some("java:11.al2023.")),
            ("ruby3.4", Some("ruby:3.4.")),
            ("dotnet8", Some("dotnet:8.")),
            ("dotnetcore3.1", Some("dotnetcore:3.1.")),
            ("go1.x", Some("go:1.")),
            ("provided.al2023", Some("provided:al2023.")),
            // An identifier that states no version line states no build of one.
            ("provided", None),
            ("nodejs", None),
            ("", None),
        ];
        for (runtime, expected) in cases {
            let actual = runtime_build(runtime);
            match expected {
                Some(opening) => {
                    let built = actual.clone().unwrap_or_default();
                    assert!(
                        built.starts_with(opening),
                        "case {runtime:?}: expected a build opening {opening:?}, got {actual:?}"
                    );
                    let build = built.trim_start_matches(opening);
                    assert!(
                        build.starts_with(BUILD_PREFIX)
                            && build[BUILD_PREFIX.len()..]
                                .parse::<u32>()
                                .is_ok_and(|number| (1..=BUILD_NUMBERS).contains(&number)),
                        "case {runtime:?}: expected a build number after {opening:?}, got \
                         {actual:?}"
                    );
                }
                None => assert_eq!(
                    actual, None,
                    "case {runtime:?}: expected no build, got {actual:?}"
                ),
            }
        }
    }

    /// Every runtime the model declares has ONE build, stable across readings, and the build
    /// of one runtime is the build of no other — the same invariant its version ARN holds.
    #[test]
    fn every_runtime_has_one_stable_build_of_its_own() {
        let mut seen = std::collections::BTreeMap::new();
        for runtime in crate::ops::RUNTIME_VALUES {
            let Some(built) = runtime_build(runtime) else {
                continue;
            };
            let again = runtime_build(runtime);
            assert_eq!(
                again.as_deref(),
                Some(built.as_str()),
                "case {runtime:?}: expected two readings to report one build, got {again:?}"
            );
            if let Some(other) = seen.insert(built.clone(), *runtime) {
                panic!("case {runtime:?}: {other:?} already reports the build {built:?}");
            }
        }
    }
}
