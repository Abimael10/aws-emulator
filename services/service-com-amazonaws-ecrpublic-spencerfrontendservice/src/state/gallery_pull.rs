// SPDX-License-Identifier: CC0-1.0

//! The PULL a container runtime makes against this gallery, answered by the registry that
//! holds the images.
//!
//! A compute service starting a container has to FETCH the image its definition names, and a
//! reference whose host is the public gallery ([`GALLERY_HOST`]) addresses THIS registry:
//! `Repository$repositoryUri` is "the URI for the repository", built from "a universal prefix
//! and the registry alias" followed by the repository name, and a container runtime appends
//! the tag or the digest it wants to that URI. So the reference reads as `<alias>`,
//! `<repository>` and either a tag or a digest ([`parse_gallery_reference`]), and whether the
//! gallery serves it is decided HERE, against the repositories and tags this registry holds,
//! rather than by whoever is starting the container.
//!
//! Only this registry's own alias is answered: an alias this registry was not assigned names
//! some other public registry ([`ComAmazonawsEcrpublicSpencerfrontendserviceState::is_this_registry`]
//! states the same thing for a request's `registryId`), which this emulator does not hold and
//! so has nothing to serve.
//!
//! A READ: deciding a pull consults the store and writes nothing, so a container start cannot
//! change what a later caller reads back from the gallery.

use super::*;

/// An image reference that addresses THIS gallery: which registry alias it names, which
/// repository, and whether it asks for a tag or a digest.
///
/// Read from the reference alone ([`parse_gallery_reference`]), so the repository a pull
/// resolves is the one the reference spells.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GalleryReference {
    /// The registry alias the reference names — the segment after the gallery host.
    pub registry_alias: String,
    /// The repository the reference names, as this registry keys one.
    pub repository_name: String,
    /// The tag the reference asks for, and `None` when it addresses a digest.
    pub image_tag: Option<String>,
    /// The digest the reference asks for, and `None` when it addresses a tag.
    pub image_digest: Option<String>,
}

impl GalleryReference {
    /// The `ImageIdentifier` this reference resolves through, which is the identifier a
    /// `DescribeImages` for the same image would carry.
    pub fn identifier(&self) -> ImageIdentifier {
        ImageIdentifier {
            image_digest: self.image_digest.clone(),
            image_tag: self.image_tag.clone(),
        }
    }
}

/// The tag a reference that names neither a tag nor a digest resolves to, which is the tag a
/// container runtime appends to a bare reference.
pub const DEFAULT_IMAGE_TAG: &str = "latest";

/// The separator a reference names a DIGEST after, as a registry client writes one
/// (`<repository>@sha256:<hex>`).
const DIGEST_SEPARATOR: &str = "@";

/// The reference `image` names when it addresses this gallery, and `None` for every other
/// reference — one in a private registry, in another registry altogether, or a bare name a
/// container runtime resolves against some other public registry.
///
/// The form is the `repositoryUri` this registry publishes with a tag or a digest appended:
/// `public.ecr.aws/<registry alias>/<repository>[:<tag>|@<digest>]`. A reference naming no tag
/// and no digest asks for [`DEFAULT_IMAGE_TAG`].
#[must_use]
pub fn parse_gallery_reference(image: &str) -> Option<GalleryReference> {
    let (host, rest) = image.trim().split_once('/')?;
    if !host.eq_ignore_ascii_case(GALLERY_HOST) {
        return None;
    }
    let (registry_alias, path) = rest.split_once('/')?;
    if registry_alias.is_empty() || path.is_empty() {
        return None;
    }
    if let Some((repository_name, digest)) = path.split_once(DIGEST_SEPARATOR) {
        if repository_name.is_empty() || digest.is_empty() {
            return None;
        }
        return Some(GalleryReference {
            registry_alias: registry_alias.to_string(),
            repository_name: repository_name.to_string(),
            image_tag: None,
            image_digest: Some(digest.to_string()),
        });
    }
    // A tag is written after the LAST `:` of the path; a repository name carries none.
    let (repository_name, image_tag) = match path.rsplit_once(':') {
        Some((repository_name, tag)) if !repository_name.is_empty() && !tag.is_empty() => {
            (repository_name, tag.to_string())
        }
        Some(_) => return None,
        None => (path, DEFAULT_IMAGE_TAG.to_string()),
    };
    Some(GalleryReference {
        registry_alias: registry_alias.to_string(),
        repository_name: repository_name.to_string(),
        image_tag: Some(image_tag),
        image_digest: None,
    })
}

/// Why this gallery did not serve a pull.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GalleryPullRefusal {
    /// Nothing in this registry answers to the reference: the message the repository's own
    /// lookup words for it.
    NotFound {
        /// Why no image was found, in this registry's own words.
        message: String,
    },
}

/// Serve the pull of `reference` from this gallery: `Ok(())` when the registry holds the image
/// the reference names, and the refusal it words otherwise.
///
/// A READ — nothing about the registry changes — so a caller may decide a pull for a container
/// it is about to start without changing what a later reader sees. An engine carrying no
/// gallery store at all says nothing about an image, so the pull is not refused on it.
pub fn serve_pull<Eng: Send + Sync + 'static>(
    engine: &Eng,
    reference: &GalleryReference,
) -> Result<(), GalleryPullRefusal> {
    let Some(state) = service_state(engine) else {
        return Ok(());
    };
    let store = state.lock();
    if !store.is_this_registry(&reference.registry_alias) {
        // A registry alias this registry was not assigned names no repository OF THIS
        // registry at all, so what the gallery holds for the reference is nothing — worded
        // as this registry words a repository name it does not hold, over the whole path the
        // reference named.
        return Err(GalleryPullRefusal::NotFound {
            message: store.repository_not_found_message(&format!(
                "{}/{}",
                reference.registry_alias, reference.repository_name
            )),
        });
    }
    let Some(repository) = store.repositories.get(&reference.repository_name) else {
        return Err(GalleryPullRefusal::NotFound {
            message: store.repository_not_found_message(&reference.repository_name),
        });
    };
    repository
        .resolve_identifier(&reference.identifier())
        .map(|_| ())
        .map_err(|problem| GalleryPullRefusal::NotFound {
            message: format!(
                "{}: {}",
                problem.failure_reason(),
                identifier_description(&reference.identifier())
            ),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One row of the reference table: the case's reference, and the reference it names —
    /// `None` for one that names no repository of THIS gallery.
    struct Row {
        image: &'static str,
        expected: Option<GalleryReference>,
    }

    /// The reference a row expects, built the way the reading answers one.
    fn names(alias: &str, repository: &str, tag: Option<&str>, digest: Option<&str>) -> GalleryReference {
        GalleryReference {
            registry_alias: alias.to_string(),
            repository_name: repository.to_string(),
            image_tag: tag.map(str::to_string),
            image_digest: digest.map(str::to_string),
        }
    }

    /// RULE: a reference is read as this gallery's only when its host is the gallery host,
    /// and the alias, repository and tag or digest are read out of the reference itself.
    #[test]
    fn a_gallery_reference_is_read_out_of_the_path() {
        let rows = [
            Row {
                image: "public.ecr.aws/alias/app:1.2",
                expected: Some(names("alias", "app", Some("1.2"), None)),
            },
            Row {
                // A reference naming no tag asks for the tag a container runtime appends.
                image: "public.ecr.aws/alias/team/app",
                expected: Some(names("alias", "team/app", Some(DEFAULT_IMAGE_TAG), None)),
            },
            Row {
                // The host is a host name, so its case does not change which registry it
                // names.
                image: "PUBLIC.ECR.AWS/alias/app",
                expected: Some(names("alias", "app", Some(DEFAULT_IMAGE_TAG), None)),
            },
            Row {
                image: "public.ecr.aws/alias/app@sha256:abc",
                expected: Some(names("alias", "app", None, Some("sha256:abc"))),
            },
            Row {
                image: "public.ecr.aws/alias",
                expected: None,
            },
            Row {
                image: "public.ecr.aws/alias/",
                expected: None,
            },
            Row {
                image: "quay.io/alias/app:1",
                expected: None,
            },
            Row {
                image: "app:1",
                expected: None,
            },
            Row {
                image: "000000000000.dkr.ecr.us-east-1.amazonaws.com/app:1",
                expected: None,
            },
        ];
        for Row { image, expected } in &rows {
            let rendered = parse_gallery_reference(image);
            assert_eq!(
                rendered, *expected,
                "case {image:?}: expected {expected:?}, got {rendered:?}"
            );
        }
    }
}
