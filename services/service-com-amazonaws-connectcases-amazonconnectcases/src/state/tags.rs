// SPDX-License-Identifier: CC0-1.0

//! The tag slice of this service's state: how an `Arn` resolves to a stored
//! record, and the tag set that record carries.
//!
//! Tags are NOT a collection of their own — every resource this service holds
//! already owns its `tags` map, which is what [`tags_output`] reports on a read
//! of the resource ([`Domain`], [`Field`], [`Template`], [`Layout`], [`Case`],
//! [`RelatedItem`]), present whatever it holds, so deleting a resource takes its
//! tags with it and no second
//! copy of a tag set can drift from the first. What the tagging operations
//! share, and what lives here, is the RESOLUTION step: an `Arn` names one
//! resource of one kind, and only a record this store actually holds is
//! taggable.

use super::*;

/// Most tags one resource carries: this crate's ceiling on request-driven
/// growth of a record's tag map, since a tagging call adds caller-chosen pairs
/// to it and nothing else would stop repeated merges growing it without end.
///
/// The NUMBER is the model's, which bounds a tag set a single call may carry at
/// fifty in both directions — the map a create of a case accepts is
/// `MutableTags`, `@length(0..50)`, and the key list one untag names is
/// `TagKeyList`, `@length(0..50)`
/// (`smithy_query(query="shape",shape="MutableTags,TagKeyList")`) — so a
/// resource held at this ceiling is one whose whole tag set can still be
/// written by one create and removed by one untag. The model states no
/// per-resource bound of its own, which is why the ceiling lives here.
pub const MAX_TAGS_PER_RESOURCE: usize = 50;

/// What a Cases ARN addresses: one resource of one kind, named by the
/// identifiers its resource path carries.
///
/// The variants are exactly the resources this service mints an ARN for
/// ([`domain_arn`], [`field_arn`], [`template_arn`], [`layout_arn`],
/// [`case_arn`], [`related_item_arn`]), so an ARN of a kind this store has no
/// record for does not parse into one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaggedResource<'a> {
    Domain {
        domain_id: &'a str,
    },
    Field {
        domain_id: &'a str,
        field_id: &'a str,
    },
    Template {
        domain_id: &'a str,
        template_id: &'a str,
    },
    Layout {
        domain_id: &'a str,
        layout_id: &'a str,
    },
    Case {
        domain_id: &'a str,
        case_id: &'a str,
    },
    RelatedItem {
        domain_id: &'a str,
        case_id: &'a str,
        related_item_id: &'a str,
    },
}

impl TaggedResource<'_> {
    /// The kind of resource this ARN addresses, which is the `resourceType` a
    /// refusal about it reports ([`ResourceKind::resource_type`]).
    pub fn kind(self) -> ResourceKind {
        match self {
            Self::Domain { .. } => ResourceKind::Domain,
            Self::Field { .. } => ResourceKind::Field,
            Self::Template { .. } => ResourceKind::Template,
            Self::Layout { .. } => ResourceKind::Layout,
            Self::Case { .. } => ResourceKind::Case,
            Self::RelatedItem { .. } => ResourceKind::RelatedItem,
        }
    }
}

/// The resource `arn` addresses, or `None` when the string is not an ARN of
/// this service naming one of its resource kinds.
///
/// `Arn` is modelled as a free-form string (`@length(1..500)`,
/// `smithy_query(query="shape",shape="Arn")`), so the boundary validator cannot
/// refuse a string that is no ARN at all: the resolution is total, and a string
/// that addresses nothing is answered as a resource this store does not hold.
///
/// The ARN's six fields, and the vendor namespace that says whose resource it
/// names, are read through the shared `service_common::arn` parse — the ONE
/// reading of a caller-supplied ARN — so this service cannot disagree with its
/// siblings about what an ARN is. Only the Cases-specific resource PATH
/// (`domain/{id}`, and a field, template, layout or case under it, with a
/// related item under its case) is read here.
pub fn tagged_resource(arn: &str) -> Option<TaggedResource<'_>> {
    let parsed = ::service_common::arn::parse_of(arn, ARN_SERVICE)?;
    let path: Vec<&str> = parsed.resource.split('/').collect();
    match path.as_slice() {
        ["domain", domain_id] => Some(TaggedResource::Domain { domain_id }),
        ["domain", domain_id, "field", field_id] => Some(TaggedResource::Field {
            domain_id,
            field_id,
        }),
        ["domain", domain_id, "template", template_id] => Some(TaggedResource::Template {
            domain_id,
            template_id,
        }),
        ["domain", domain_id, "layout", layout_id] => Some(TaggedResource::Layout {
            domain_id,
            layout_id,
        }),
        ["domain", domain_id, "case", case_id] => {
            Some(TaggedResource::Case { domain_id, case_id })
        }
        ["domain", domain_id, "case", case_id, "related-item", related_item_id] => {
            Some(TaggedResource::RelatedItem {
                domain_id,
                case_id,
                related_item_id,
            })
        }
        _ => None,
    }
}

/// The modelled 404 a tagging call on an ARN that addresses no resource this
/// store holds gets.
///
/// `resourceId` is the ARN the request named — the only identifier a tagging
/// call carries — and `resourceType` the kind that ARN addresses, or
/// [`UNNAMED_RESOURCE_KIND`] for a value that addresses no resource kind of this
/// service at all; the member is required, so it is never reported empty. The
/// sentence is the service's generic one ([`NotFoundWording::Generic`]), as for
/// every other resource this service words no sentence of its own for.
pub fn tagged_resource_not_found(arn: &str) -> ResourceNotFoundException {
    match tagged_resource(arn) {
        Some(resource) => not_found(resource.kind(), arn),
        None => ResourceNotFoundException {
            message: RESOURCE_NOT_FOUND_MESSAGE.to_string(),
            resource_id: arn.to_string(),
            resource_type: UNNAMED_RESOURCE_KIND.to_string(),
        },
    }
}

/// What a refusal names the resource an ARN addresses when the ARN addresses no
/// resource kind of this service at all: a value that is no ARN, an ARN of
/// another vendor's namespace, or one whose resource path names no kind this
/// service addresses. `resourceType` is a REQUIRED member of the modelled
/// refusal, so it reports this generic word rather than nothing.
pub const UNNAMED_RESOURCE_KIND: &str = "resource";

/// The modelled 400 for a merge that would leave one resource carrying more
/// than [`MAX_TAGS_PER_RESOURCE`] tags; `requested` is how many tags the
/// resource would end up with.
///
/// A DIFFERENT rule from [`list_too_long`], which bounds how long a list ONE
/// REQUEST may carry: this bounds how many tags one RESOURCE ends up carrying
/// after the merge, so it has a wording of its own. Nothing recorded pins the
/// sentence AWS words this ceiling with, so it is written in this service's own
/// validation voice — the member that is wrong, the count the request would
/// leave the resource with and the ceiling, every variable part taken from the
/// request and the stored record and none of them from any one run.
pub fn too_many_tags(arn: &str, requested: usize) -> ValidationException {
    ValidationException {
        message: format!(
            "tags would leave {arn} carrying {requested} tags; at most \
             {MAX_TAGS_PER_RESOURCE} are accepted per resource"
        ),
    }
}

impl ComAmazonawsConnectcasesAmazonconnectcasesState {
    /// The stored tag set of the resource `arn` addresses, or `None` when this
    /// store holds no such resource.
    ///
    /// Resolution is by the record's OWN ARN: the ARN's resource path picks the
    /// collection and the record within it, and the record's stored ARN must
    /// then equal the one asked for — which is what makes the partition, region
    /// and account segments count, so a resource created in one region is not
    /// addressable through another region's ARN.
    ///
    /// A SOFT-deleted field, template or layout is still REPORTED by this
    /// service — `BatchGetField`, `GetTemplate` and `GetLayout` all answer a
    /// deleted record, tags included — so a read of its tag set resolves it too,
    /// and the tags a resource carried stay readable for exactly as long as the
    /// resource itself does. A WRITE through a tombstone is what the service
    /// refuses ([`Self::resource_tags_mut`]).
    pub fn resource_tags(&self, arn: &str) -> Option<&HashMap<String, String>> {
        match tagged_resource(arn)? {
            TaggedResource::Domain { domain_id } => self
                .domains
                .get(domain_id)
                .filter(|domain| domain.domain_arn == arn)
                .map(|domain| &domain.tags),
            TaggedResource::Field {
                domain_id,
                field_id,
            } => self
                .domains
                .get(domain_id)?
                .fields
                .get(field_id)
                .filter(|field| field.field_arn == arn)
                .map(|field| &field.tags),
            TaggedResource::Template {
                domain_id,
                template_id,
            } => self
                .domains
                .get(domain_id)?
                .templates
                .get(template_id)
                .filter(|template| template.template_arn == arn)
                .map(|template| &template.tags),
            TaggedResource::Layout {
                domain_id,
                layout_id,
            } => self
                .domains
                .get(domain_id)?
                .layouts
                .get(layout_id)
                .filter(|layout| layout.layout_arn == arn)
                .map(|layout| &layout.tags),
            TaggedResource::Case { domain_id, case_id } => self
                .domains
                .get(domain_id)?
                .cases
                .get(case_id)
                .filter(|case| case.case_arn == arn)
                .map(|case| &case.tags),
            TaggedResource::RelatedItem {
                domain_id,
                case_id,
                related_item_id,
            } => self
                .domains
                .get(domain_id)?
                .cases
                .get(case_id)?
                .related_items
                .get(related_item_id)
                .filter(|item| item.related_item_arn == arn)
                .map(|item| &item.tags),
        }
    }

    /// The stored tag set of the resource `arn` addresses, writable, or `None`
    /// when this store holds no such resource a write can go through.
    ///
    /// Resolves as [`Self::resource_tags`] does, with one difference: a
    /// SOFT-deleted field, template or layout is a tombstone rather than a
    /// resource a caller can take a new reference to, and every other write of
    /// this service refuses one ([`Domain::field_mut`], [`Domain::template`] —
    /// `UpdateField`, `UpdateTemplate` and `UpdateLayout` all go through them),
    /// so a tag write through one is refused too.
    pub fn resource_tags_mut(&mut self, arn: &str) -> Option<&mut HashMap<String, String>> {
        match tagged_resource(arn)? {
            TaggedResource::Domain { domain_id } => self
                .domains
                .get_mut(domain_id)
                .filter(|domain| domain.domain_arn == arn)
                .map(|domain| &mut domain.tags),
            TaggedResource::Field {
                domain_id,
                field_id,
            } => self
                .domains
                .get_mut(domain_id)?
                .fields
                .get_mut(field_id)
                .filter(|field| !field.deleted && field.field_arn == arn)
                .map(|field| &mut field.tags),
            TaggedResource::Template {
                domain_id,
                template_id,
            } => self
                .domains
                .get_mut(domain_id)?
                .templates
                .get_mut(template_id)
                .filter(|template| !template.deleted && template.template_arn == arn)
                .map(|template| &mut template.tags),
            TaggedResource::Layout {
                domain_id,
                layout_id,
            } => self
                .domains
                .get_mut(domain_id)?
                .layouts
                .get_mut(layout_id)
                .filter(|layout| !layout.deleted && layout.layout_arn == arn)
                .map(|layout| &mut layout.tags),
            TaggedResource::Case { domain_id, case_id } => self
                .domains
                .get_mut(domain_id)?
                .cases
                .get_mut(case_id)
                .filter(|case| case.case_arn == arn)
                .map(|case| &mut case.tags),
            TaggedResource::RelatedItem {
                domain_id,
                case_id,
                related_item_id,
            } => self
                .domains
                .get_mut(domain_id)?
                .cases
                .get_mut(case_id)?
                .related_items
                .get_mut(related_item_id)
                .filter(|item| item.related_item_arn == arn)
                .map(|item| &mut item.tags),
        }
    }

    /// Merge `tags` into the tag set of the resource `arn` addresses: a key the
    /// resource already carries takes the request's value, the rest are added,
    /// and nothing else on the record is touched.
    ///
    /// The merged size is counted BEFORE anything is written, so a request
    /// refused for taking the resource past [`MAX_TAGS_PER_RESOURCE`] tags
    /// changes nothing.
    pub fn merge_resource_tags(
        &mut self,
        arn: &str,
        tags: &HashMap<String, String>,
    ) -> Result<(), TagWriteError> {
        let stored = self
            .resource_tags_mut(arn)
            .ok_or(TagWriteError::NotFound)?;
        let merged = stored.len() + tags.keys().filter(|key| !stored.contains_key(*key)).count();
        if merged > MAX_TAGS_PER_RESOURCE {
            return Err(TagWriteError::TooManyTags(merged));
        }
        for (key, value) in tags {
            stored.insert(key.clone(), value.clone());
        }
        Ok(())
    }

    /// Remove every key in `tag_keys` from the tag set of the resource `arn`
    /// addresses: a key the resource carries is dropped, a key it does not
    /// carry leaves the set as it was, and nothing else on the record is
    /// touched.
    ///
    /// Resolves exactly as [`Self::merge_resource_tags`] does — a tag write of
    /// either direction goes through [`Self::resource_tags_mut`], so the two
    /// cannot disagree about which records a write goes through — and answers
    /// `None` for an ARN this store holds no such resource for. Removing keys
    /// only ever SHRINKS the set, so [`MAX_TAGS_PER_RESOURCE`] cannot be
    /// crossed here and this write has no second way to fail.
    pub fn remove_resource_tags(&mut self, arn: &str, tag_keys: &[String]) -> Option<()> {
        let stored = self.resource_tags_mut(arn)?;
        for key in tag_keys {
            stored.remove(key);
        }
        Some(())
    }
}

/// Why a tag write was refused: the resource could not be resolved, or the
/// merge would leave it carrying more tags than [`MAX_TAGS_PER_RESOURCE`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagWriteError {
    NotFound,
    /// How many tags the resource would have ended up carrying.
    TooManyTags(usize),
}
