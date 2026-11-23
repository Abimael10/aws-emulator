// SPDX-License-Identifier: CC0-1.0

//! The ENTRY POINT an image declares, which is the program a container whose definition
//! declares none runs.
//!
//! # The rule this module implements
//!
//! A container runs an argument vector, and the vector is the ENTRY POINT followed by the
//! COMMAND: `ContainerDefinition$entryPoint` is "the entry point that's passed to the
//! container" and `ContainerDefinition$command` "the command that's passed to the container.
//! ... If there are multiple arguments, each argument is a separated string in the array",
//! with that member's own documentation adding that a command overrides "the default command
//! from the Docker image". So each half has a default the IMAGE declares, and a definition
//! states only the halves it overrides: a container that declares a `command` on an image
//! whose own entry point is a program runs THAT program with the declared command as its
//! arguments, exactly as the container runtime exec's it.
//!
//! This is the catalog of the images whose declared entry point this emulator knows, beside
//! [`super::container_servers::image_declared_command`], which is the other half of the same
//! rule — the command an image declares for a container that overrides neither. An image
//! outside both catalogs declares nothing this emulator knows, so nothing is invented for it
//! and such a container's program is the vector its definition states and nothing more.

use super::container_servers::image_repository_path;

/// One image whose own ENTRY POINT this emulator knows: where it is published, and the
/// program its `ENTRYPOINT` names.
#[derive(Debug, Clone, Copy)]
pub struct ImageEntryPoint {
    /// The REPOSITORIES the image is published under, as the path of a reference naming it
    /// carries past the registry host ([`image_repository_path`]): a container of one of them
    /// runs that image, and a repository of someone else's that only happens to end in the
    /// same word does not.
    pub repositories: &'static [&'static str],
    /// The entry point that image declares, as the argument vector it is exec'd as. Each word
    /// names its program the way `PATH` resolution finds it, which is the name the session
    /// performing a container's program takes ([`super::container_shell`]).
    pub entry_point: &'static [&'static str],
}

/// The images whose declared entry point this emulator knows.
///
/// The AWS CLI's own official image is one: its entry point IS the CLI ("the official AWS CLI
/// version 2 Docker image's entry point is the `aws` program, so a `docker run` of it takes
/// the CLI's own arguments" — the AWS CLI User Guide's account of running the image), which
/// is why a container of it is given `["s3", "cp", …]` rather than a vector naming a program.
/// It is published in the public gallery under its own `aws-cli` namespace and on the
/// registry a reference naming no host resolves against under `amazon/aws-cli`; both
/// spellings name the one image, so both carry its entry point. The program is named by the
/// one name this workspace dispatches the CLI under
/// ([`::service_common::aws_s3_cli::CLI_PROGRAM`]), so a container of that image reaches the
/// same program a definition naming it by hand does.
pub const IMAGE_ENTRY_POINTS: &[ImageEntryPoint] = &[ImageEntryPoint {
    repositories: &["aws-cli/aws-cli", "amazon/aws-cli"],
    entry_point: &[::service_common::aws_s3_cli::CLI_PROGRAM],
}];

/// The entry point the IMAGE `image` declares, for a container whose definition declares none
/// — and `None` for every reference this emulator knows no entry point of, whose program it
/// therefore does not invent.
#[must_use]
pub fn image_declared_entry_point(image: &str) -> Option<Vec<String>> {
    let repository = image_repository_path(image);
    IMAGE_ENTRY_POINTS
        .iter()
        .find(|held| held.repositories.contains(&repository))
        .map(|held| {
            held.entry_point
                .iter()
                .map(|&word| word.to_string())
                .collect()
        })
}

#[cfg(test)]
mod tests {
    use super::{image_declared_entry_point, IMAGE_ENTRY_POINTS};

    /// RULE: the entry point an image declares is decided by the REPOSITORY a reference names,
    /// whichever way that reference spells which image of it to fetch, and no entry point is
    /// claimed for a repository this emulator knows none of.
    ///
    /// The rows differ along what the rule generalises over: the registry the reference
    /// addresses (the public gallery's own namespace and the registry a reference naming no
    /// host resolves against), how it selects an image (a release tag, a moving tag, none at
    /// all, a digest), and whether the repository is one of the catalog's at all — including a
    /// repository of somebody else's whose last segment is the same word.
    #[test]
    fn the_repository_a_reference_names_decides_the_images_entry_point() {
        // (case, the reference, the entry point it declares)
        let rows: [(&str, &str, Option<Vec<String>>); 9] = [
            (
                "the gallery's own namespace, on a release tag",
                "public.ecr.aws/aws-cli/aws-cli:2.36.44",
                Some(vec!["aws".to_string()]),
            ),
            (
                "the same repository on its moving tag",
                "public.ecr.aws/aws-cli/aws-cli:latest",
                Some(vec!["aws".to_string()]),
            ),
            (
                "the same repository naming no tag at all",
                "public.ecr.aws/aws-cli/aws-cli",
                Some(vec!["aws".to_string()]),
            ),
            (
                "the same repository addressed by digest",
                "public.ecr.aws/aws-cli/aws-cli@sha256:\
                 0000000000000000000000000000000000000000000000000000000000000000",
                Some(vec!["aws".to_string()]),
            ),
            (
                "the mirrored registry's spelling, which names no host",
                "amazon/aws-cli:2",
                Some(vec!["aws".to_string()]),
            ),
            (
                "that spelling with the host written out",
                "docker.io/amazon/aws-cli:2",
                Some(vec!["aws".to_string()]),
            ),
            (
                "a repository of somebody else's ending in the same word",
                "registry.example.com/simparity/aws-cli:1",
                None,
            ),
            (
                "a web server image, whose declared COMMAND is the other catalog's",
                "public.ecr.aws/docker/library/nginx:1.29",
                None,
            ),
            (
                "an image this emulator knows nothing of",
                "registry.example.com/simparity/batch-worker:1",
                None,
            ),
        ];
        for (case, reference, expected) in rows {
            let answered = image_declared_entry_point(reference);
            assert_eq!(
                answered, expected,
                "case {case:?}: expected {expected:?} for {reference:?}, got {answered:?}"
            );
        }
    }

    /// RULE: every entry of the catalog names at least one repository and at least one word of
    /// program, so no entry can leave a container with an empty vector for a program.
    #[test]
    fn every_catalog_entry_names_a_repository_and_a_program() {
        for held in IMAGE_ENTRY_POINTS {
            assert!(
                !held.repositories.is_empty(),
                "case {held:?}: an entry has to name where its image is published"
            );
            assert!(
                held.entry_point.iter().all(|word| !word.is_empty()),
                "case {held:?}: an entry point word has to name something"
            );
            assert!(
                !held.entry_point.is_empty(),
                "case {held:?}: an entry has to name the program its image exec's"
            );
        }
    }
}
