// SPDX-License-Identifier: CC0-1.0

//! The RELEASE the graph engine runs for a published engine version, for the control plane
//! that describes the version and the data plane that reports the engine serving it.
//!
//! The graph engine publishes an engine version as four numeric parts (`1.4.8.0`) and a
//! RELEASE of that version as the version plus the build of it that ships
//! (`1.4.8.0.R1`): a version patched after it first shipped publishes a later build of
//! itself under the same version number. The control plane's version description names
//! the release, and the engine's own status report names the release it is running, so
//! both read the build from here rather than each carrying its own answer — while the two
//! tables drifted, one plane named a release the other said was not running.

/// The build of a release that has not been patched since it shipped: its first.
pub const FIRST_BUILD: &str = "R1";

/// The build the engine publishes for a version whose current release is NOT its first.
///
/// Source: the engine's published engine releases, which name a release `<version>.R<build>`
/// and carry a later build for a version patched after it shipped — the 1.1.1.0 version's
/// published release is `1.1.1.0.R7` and the 1.2.0.0 version's is `1.2.0.0.R4`. Only a
/// version whose build is not the first needs a row: every other version publishes
/// [`FIRST_BUILD`], which is also the build this emulator stands a cluster of that version
/// up on.
///
/// The rows are this emulator's published SAMPLE of that release history, as the engine
/// version catalogue that enumerates the versions is: a version this table carries no row
/// for is described by its first build, which is the release this emulator serves for it —
/// so a version AWS has since published a later build of is named here as the build this
/// emulator runs, and a row added from the engine's release notes moves both planes at once.
const LATER_BUILDS: &[(&str, &str)] = &[("1.1.1.0", "R7"), ("1.2.0.0", "R4")];

/// The BUILD the engine publishes for `engine_version`: its own where [`LATER_BUILDS`]
/// carries one, else [`FIRST_BUILD`].
pub fn build_of(engine_version: &str) -> &'static str {
    LATER_BUILDS
        .iter()
        .find(|(version, _)| version.eq_ignore_ascii_case(engine_version))
        .map_or(FIRST_BUILD, |(_, build)| *build)
}

/// The RELEASE of `engine_version`: the version with the build the engine publishes for it
/// after it (`1.1.1.0` -> `1.1.1.0.R7`).
///
/// A value that is not four numeric parts is answered as it stands: it already names a
/// release, or it names no version of this engine at all, and either way appending a build
/// would name a release that does not exist.
pub fn release(engine_version: &str) -> String {
    if !is_an_engine_version(engine_version) {
        return engine_version.to_string();
    }
    format!("{engine_version}.{}", build_of(engine_version))
}

/// Whether `value` is written the way this engine writes an engine version: four numeric
/// parts.
fn is_an_engine_version(value: &str) -> bool {
    let parts: Vec<&str> = value.split('.').collect();
    parts.len() == 4
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A version's release is that version plus the build the engine publishes for it, and a
    /// value that is no engine version keeps its own text.
    #[test]
    fn a_release_names_the_version_and_its_published_build() {
        // Rows differ along what the rule generalises over: a version with a later
        // published build, versions whose build is the first, a version of each published
        // line, a value that already carries a build, and values that name no version of
        // this engine at all.
        let cases: &[(&str, &str)] = &[
            ("1.1.1.0", "1.1.1.0.R7"),
            ("1.2.0.0", "1.2.0.0.R4"),
            ("1.1.2.0", "1.1.2.0.R1"),
            ("1.2.1.2", "1.2.1.2.R1"),
            ("1.3.4.0", "1.3.4.0.R1"),
            ("1.4.8.0", "1.4.8.0.R1"),
            // Already a release: answered as it stands rather than given a second build.
            ("1.4.8.0.R2", "1.4.8.0.R2"),
            // Not four numeric parts, so no release of this engine.
            ("1.4.8", "1.4.8"),
            ("8.0.40", "8.0.40"),
            ("", ""),
        ];
        for (version, expected) in cases {
            let release = release(version);
            assert_eq!(
                release, *expected,
                "case {version:?}: expected release {expected:?}, got {release:?}"
            );
        }
    }

    /// A version's build is read off the table whatever the case of the version text, as a
    /// version read off a request is.
    #[test]
    fn a_versions_build_is_read_whatever_its_case() {
        let cases: &[(&str, &str)] = &[
            ("1.1.1.0", "R7"),
            ("1.2.0.0", "R4"),
            ("1.3.0.0", FIRST_BUILD),
            ("1.4.8.0", FIRST_BUILD),
        ];
        for (version, expected) in cases {
            let build = build_of(version);
            assert_eq!(
                build, *expected,
                "case {version:?}: expected build {expected:?}, got {build:?}"
            );
            assert_eq!(
                build_of(&version.to_ascii_uppercase()),
                *expected,
                "case {version:?}: a version's build does not depend on its case"
            );
        }
    }
}
