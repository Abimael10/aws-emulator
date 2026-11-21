// SPDX-License-Identifier: CC0-1.0

//! The repositories the gallery AWS serves PUBLISHES ITSELF, and which releases of each this
//! emulator holds.
//!
//! # Why anything is held here at all
//!
//! The gallery AWS serves carries one public registry per publishing account, each under its
//! own alias, and this emulator holds a STORE for exactly one of them — this account's
//! ([`super::gallery_pull`]). A reference into any other alias is therefore a reference into a
//! registry whose repositories no store here lists, which is why nothing is decided against
//! such a fetch: an absent store is not evidence that an image is missing.
//!
//! That is not the whole truth for the registries AWS publishes in the gallery itself. The
//! `docker` alias is the gallery's mirror of the Docker Official Images, and a compute service
//! of this emulator does not merely pass such a reference along: it RUNS the image's own
//! program, which it can only do for an image it holds (the servers and the multi-call binary
//! the Amazon ECS crate performs a container's command with). An image this emulator holds is
//! an image whose RELEASES it holds too — the tags that repository publishes — so a reference
//! into one of them IS decided here: a tag the repository publishes is served, and a tag it
//! publishes no release under resolves to no image, exactly as a repository of this registry's
//! own does. A repository, or an alias, this emulator holds no image of is left undecided as
//! before.
//!
//! # What the entries state, and where they come from
//!
//! Each entry is the repository as the gallery PUBLISHES it — the alias it is published under,
//! its repository name and the tags its gallery listing carries (`gallery.ecr.aws/docker/…`,
//! which mirrors the Docker Official Images' own tag lists; the same listing a client reads
//! with `DescribeImageTags` for a repository of this registry). Nothing is computed from a
//! request and no tag is invented: a release this emulator does not hold is one the gallery
//! here holds no image for.
//!
//! A reference that addresses a DIGEST is not answered: a digest names content by its own
//! hash rather than a release the repository published, so the tags a repository publishes
//! state nothing about it.
//!
//! A READ: deciding a pull consults these entries and writes nothing, so a container start
//! cannot change what a later caller reads back.

use super::gallery_pull::GalleryReference;

/// One repository the gallery AWS serves publishes, whose image this emulator holds.
#[derive(Debug, Clone, Copy)]
pub struct PublishedRepository {
    /// The registry alias the repository is published under, which is the segment a reference
    /// carries after the gallery host.
    pub registry_alias: &'static str,
    /// The repository name, as a reference spells it after the alias.
    pub repository_name: &'static str,
    /// The tags the repository's gallery listing publishes a release under.
    pub image_tags: &'static [&'static str],
}

/// The tags the gallery's mirror of the BusyBox official image publishes.
///
/// The image's own release tags (its `1.x` versions, each also reachable under the version
/// family it leads), the moving `latest`/`stable`/`unstable` channels, and the libc variants
/// that image documents (`glibc`, `musl`, `uclibc`), alone and on a channel.
const BUSYBOX_TAGS: &[&str] = &[
    "latest",
    "stable",
    "unstable",
    "1",
    "1.36",
    "1.36.1",
    "1.37",
    "1.37.0",
    "glibc",
    "musl",
    "uclibc",
    "stable-glibc",
    "stable-musl",
    "stable-uclibc",
    "unstable-glibc",
    "unstable-musl",
    "unstable-uclibc",
];

/// The tags the gallery's mirror of the nginx official image publishes: its `1.x` release
/// families, the two branches that image names (`mainline`, `stable`), the Alpine-based
/// variants of each and the `perl` build, plus `latest`.
const NGINX_TAGS: &[&str] = &[
    "latest",
    "mainline",
    "stable",
    "alpine",
    "alpine-slim",
    "mainline-alpine",
    "stable-alpine",
    "perl",
    "1",
    "1.26",
    "1.26-alpine",
    "1.27",
    "1.27-alpine",
    "1.28",
    "1.28-alpine",
    "1.29",
    "1.29-alpine",
];

/// The tags the gallery's mirror of the Apache HTTP Server official image publishes: its
/// `2.4` release family, the Alpine-based variant of it, and `latest`.
const HTTPD_TAGS: &[&str] = &["latest", "alpine", "2", "2.4", "2.4-alpine"];

/// The repositories of the gallery AWS serves whose image this emulator holds.
///
/// All three are Docker Official Images, published in the gallery under the `docker` alias
/// with the `library/` path those images carry, and all three are images whose own program a
/// container of this emulator performs: the multi-call binary a small image's command invokes
/// its applets through, and the two web servers an image ships
/// (`::service_com_amazonaws_ecs_amazonec2containerservicev20141113::state::container_servers`
/// holds that reading, which is also where the repositories each server's image is published
/// under are listed).
pub const PUBLISHED_REPOSITORIES: &[PublishedRepository] = &[
    PublishedRepository {
        registry_alias: DOCKER_OFFICIAL_IMAGES_ALIAS,
        repository_name: "library/busybox",
        image_tags: BUSYBOX_TAGS,
    },
    PublishedRepository {
        registry_alias: DOCKER_OFFICIAL_IMAGES_ALIAS,
        repository_name: "library/nginx",
        image_tags: NGINX_TAGS,
    },
    PublishedRepository {
        registry_alias: DOCKER_OFFICIAL_IMAGES_ALIAS,
        repository_name: "library/httpd",
        image_tags: HTTPD_TAGS,
    },
];

/// The alias the gallery publishes its mirror of the Docker Official Images under, which is
/// the registry every entry above belongs to.
const DOCKER_OFFICIAL_IMAGES_ALIAS: &str = "docker";

/// What this catalogue says about one reference into a repository it holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishedImage {
    /// The repository publishes a release under the tag the reference asks for.
    Published,
    /// The repository publishes no release under it, so the reference resolves to no image.
    NotPublished,
}

/// The entry for the repository `repository_name` of the registry `registry_alias`, or `None`
/// for every repository and alias this emulator holds no image of.
#[must_use]
pub fn published_repository(
    registry_alias: &str,
    repository_name: &str,
) -> Option<&'static PublishedRepository> {
    PUBLISHED_REPOSITORIES.iter().find(|published| {
        published.registry_alias == registry_alias && published.repository_name == repository_name
    })
}

/// Whether the gallery publishes the image `reference` names, and `None` where this emulator
/// holds no image of that repository — in which case nothing is decided about the reference,
/// as for every other registry it holds no store for.
///
/// A reference addressing a DIGEST is not answered either (see the module docs).
#[must_use]
pub fn published_image(reference: &GalleryReference) -> Option<PublishedImage> {
    let published = published_repository(&reference.registry_alias, &reference.repository_name)?;
    let tag = reference.image_tag.as_deref()?;
    Some(if published.image_tags.contains(&tag) {
        PublishedImage::Published
    } else {
        PublishedImage::NotPublished
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reference a case asks about, built as the gallery's own reading answers one.
    fn reference(alias: &str, repository: &str, tag: Option<&str>, digest: Option<&str>) -> GalleryReference {
        GalleryReference {
            registry_alias: alias.to_string(),
            repository_name: repository.to_string(),
            image_tag: tag.map(str::to_string),
            image_digest: digest.map(str::to_string),
        }
    }

    /// RULE: a reference into a repository of the gallery whose image this emulator holds is
    /// decided by the tags that repository publishes — a published tag names an image and any
    /// other tag names none — while a repository or an alias no image is held of, and a
    /// reference that addresses a digest, are not answered at all.
    #[test]
    fn the_published_tags_of_a_held_repository_decide_a_reference() {
        // (case, the reference, what the catalogue says)
        let rows: &[(&str, GalleryReference, Option<PublishedImage>)] = &[
            (
                "a release of a held repository",
                reference("docker", "library/busybox", Some("1.36"), None),
                Some(PublishedImage::Published),
            ),
            (
                "the moving channel of a held repository",
                reference("docker", "library/nginx", Some("latest"), None),
                Some(PublishedImage::Published),
            ),
            (
                "a variant release of a held repository",
                reference("docker", "library/httpd", Some("2.4-alpine"), None),
                Some(PublishedImage::Published),
            ),
            (
                "a tag the held repository publishes no release under",
                reference("docker", "library/busybox", Some("no-such-release"), None),
                Some(PublishedImage::NotPublished),
            ),
            (
                "a version the held repository publishes no release under",
                reference("docker", "library/nginx", Some("0.1"), None),
                Some(PublishedImage::NotPublished),
            ),
            (
                "a repository of that registry no image is held of",
                reference("docker", "library/python", Some("3.12"), None),
                None,
            ),
            (
                "an alias no image is held of",
                reference("amazonlinux", "amazonlinux", Some("2023"), None),
                None,
            ),
            (
                "a digest, which names content rather than a release",
                reference("docker", "library/busybox", None, Some("sha256:abc")),
                None,
            ),
        ];
        for (case, asked, expected) in rows {
            let answered = published_image(asked);
            assert_eq!(
                answered, *expected,
                "case {case:?}: expected {expected:?}, got {answered:?}"
            );
        }
    }

    /// Every entry has to be reachable by the alias and repository a reference spells, and
    /// carry at least one tag — an entry with none would hold no release at all and so refuse
    /// every reference into a repository this emulator does hold the image of.
    #[test]
    fn every_published_entry_is_reachable_and_holds_releases() {
        for published in PUBLISHED_REPOSITORIES {
            let found = published_repository(published.registry_alias, published.repository_name);
            assert!(
                found.is_some(),
                "case {}/{}: the entry must be reachable by the reference's own spelling",
                published.registry_alias,
                published.repository_name
            );
            assert!(
                !published.image_tags.is_empty(),
                "case {}/{}: expected at least one published tag, got none",
                published.registry_alias,
                published.repository_name
            );
        }
    }
}
