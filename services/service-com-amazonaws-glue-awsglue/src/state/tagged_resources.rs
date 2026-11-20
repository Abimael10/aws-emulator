// SPDX-License-Identifier: CC0-1.0

//! The Glue resources a tagging call addresses BY ARN: which kinds carry a tag set, how a
//! `ResourceArn` resolves to one of them, and the one record every tag read and every tag
//! write lands in.
//!
//! A tagging call's `ResourceArn` is a `GlueResourceArn`, whose modelled constraint is the
//! prefix every Glue ARN shares and nothing more — it names no resource TYPE — so the
//! resolution here is over the resource portion of the ARN: the resource-type segment picks
//! the kind, the rest of it names the resource of that kind, and whether this store holds
//! that resource is the accessors' answer. Every Glue resource whose record carries a tag
//! set is one of the kinds, so a registry, a schema set, a session, a workflow, a trigger
//! and the rest are tagged and read back exactly as a database or a job definition is.

use super::*;

/// One kind of Glue resource that carries its OWN tag set, and so can be named by a
/// tagging call's `ResourceArn`.
///
/// The kinds are the ones whose record in this store holds a tag set, which is every Glue
/// resource whose create models a `Tags` map ("The tags to use with this..." —
/// `CreateDatabase$Tags`, `CreateConnection$Tags`, `CreateCrawler$Tags`, `CreateJob$Tags`,
/// `CreateTrigger$Tags`, `CreateWorkflow$Tags`, `CreateBlueprint$Tags`,
/// `CreateRegistry$Tags`, `CreateSchema$Tags`, `CreateSession$Tags`, `CreateCatalog$Tags`,
/// `CreateMLTransform$Tags`, `CreateUsageProfile$Tags`, `CreateCustomEntityType$Tags`,
/// `CreateDataQualityRuleset$Tags`) and whose resource this emulator holds.
///
/// A Glue resource whose create models NO `Tags` map carries no tag set to report: a
/// catalog TABLE (`CreateTable` models none), a table version, a user defined function and
/// a partition are each addressed by an ARN of their own, and none of them is a kind here.
/// Neither is a kind whose resource this emulator holds no record of at all — a development
/// endpoint, whose create is still a scaffold — because there is no resource lifecycle for
/// an ARN of one to resolve against; resolving those belongs with the operations that
/// create them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaggedResourceKind {
    /// `blueprint/<name>`.
    Blueprint,
    /// `catalog/<name>`: a catalog `CreateCatalog` registered INSIDE a Data Catalog, not the
    /// account's Data Catalog itself (whose ARN carries no name after the segment).
    Catalog,
    /// `connection/<name>`.
    Connection,
    /// `crawler/<name>`.
    Crawler,
    /// `customEntityType/<name>`.
    CustomEntityType,
    /// `database/<name>`.
    Database,
    /// `dataQualityRuleset/<name>`.
    DataQualityRuleset,
    /// `job/<name>`.
    Job,
    /// `mlTransform/<transform-id>`.
    MlTransform,
    /// `registry/<name>`.
    Registry,
    /// `schema/<registry-name>/<schema-name>`: a schema set lives inside a registry, so its
    /// resource portion carries BOTH names.
    Schema,
    /// `session/<session-id>`.
    Session,
    /// `trigger/<name>`.
    Trigger,
    /// `usageProfile/<name>`.
    UsageProfile,
    /// `workflow/<name>`.
    Workflow,
}

/// The ARN resource-type segment each taggable kind carries: the catalog
/// [`parse_tagged_resource`] resolves an ARN against and [`TaggedResourceKind::arn`]
/// mints one from.
///
/// Source: the resource portion of each resource's own Glue ARN, which the model spells
/// where it spells one at all (`CreateSchema$RegistryId`: "The ARN format for the same will
/// be: arn:aws:glue:us-east-2::registry/default-registry…", and [`registry_arn`] and
/// [`schema_arn`] beside it), and which is otherwise the resource type AWS names a Glue
/// resource of that kind by — the camel-cased type name (`mlTransform`, `usageProfile`,
/// `customEntityType`, `dataQualityRuleset`). The model states only the prefix every Glue
/// ARN shares (`GlueResourceArn`'s pattern, see [`ARN_NAMESPACE`]), so the segments are
/// listed here once and every mint and every lookup goes through this one table.
pub const TAGGED_RESOURCE_TYPES: [(&str, TaggedResourceKind); 15] = [
    ("blueprint", TaggedResourceKind::Blueprint),
    ("catalog", TaggedResourceKind::Catalog),
    ("connection", TaggedResourceKind::Connection),
    ("crawler", TaggedResourceKind::Crawler),
    ("customEntityType", TaggedResourceKind::CustomEntityType),
    ("database", TaggedResourceKind::Database),
    ("dataQualityRuleset", TaggedResourceKind::DataQualityRuleset),
    ("job", TaggedResourceKind::Job),
    ("mlTransform", TaggedResourceKind::MlTransform),
    ("registry", TaggedResourceKind::Registry),
    ("schema", TaggedResourceKind::Schema),
    ("session", TaggedResourceKind::Session),
    ("trigger", TaggedResourceKind::Trigger),
    ("usageProfile", TaggedResourceKind::UsageProfile),
    ("workflow", TaggedResourceKind::Workflow),
];

/// How many tags one resource's tag set may hold at once.
///
/// Source: the modelled `TagsMap` bound (len 0..50), which is the shape BOTH a tag
/// write's `TagsToAdd` and `GetTags`'s `Tags` target — so a tag set past this many
/// entries could not be reported by the read that serves it. A write that would carry a
/// resource past it is refused instead of stored.
pub const MAX_TAGS_PER_RESOURCE: usize = 50;

impl TaggedResourceKind {
    /// This kind's ARN resource-type segment, from [`TAGGED_RESOURCE_TYPES`].
    pub fn arn_segment(self) -> &'static str {
        TAGGED_RESOURCE_TYPES
            .iter()
            .find(|(_, kind)| *kind == self)
            .map(|(segment, _)| *segment)
            // Every variant has a row above, so this is unreachable; answering the
            // service namespace rather than panicking keeps a request path alive.
            .unwrap_or(ARN_NAMESPACE)
    }

    /// The ARN of the resource `name` of this kind, as served in `region`.
    ///
    /// `name` is the resource portion AFTER the type segment, which for a schema set is
    /// both of its names (`<registry>/<schema>`).
    ///
    /// The account segment is this emulator's own ([`::engine::emulator_arn`]), which is
    /// also the only `CatalogId` a catalog resource can live under here (see
    /// [`is_known_catalog`]), so a minted ARN and a stored record always agree on whose
    /// resource it is.
    pub fn arn(self, region: &str, name: &str) -> String {
        ::engine::emulator_arn(
            ARN_NAMESPACE,
            region,
            &format!("{}/{name}", self.arn_segment()),
        )
    }

    /// Whether this kind's names are FOLDED the way the Hive-compatible Data Catalog folds
    /// them ([`fold_name`]), so an ARN resolves the resource whatever case it spells the
    /// name in.
    ///
    /// The two kinds the catalog folds are the ones it keys by a folded name: a database
    /// ([`ComAmazonawsGlueAwsglueState::catalogs`]) and a catalog resource
    /// ([`ComAmazonawsGlueAwsglueState::catalog_definition`]). Every other kind is keyed by
    /// the name exactly as its create sent it, because the model documents no fold for it.
    fn folds_name(self) -> bool {
        matches!(self, Self::Database | Self::Catalog)
    }

    /// How many `/`-separated parts the resource portion carries after the type segment:
    /// two for a schema set, which is named by its registry and itself, one for every other
    /// kind, whose name is a single `NameString` (a shape whose pattern admits no `/`).
    fn name_parts(self) -> usize {
        match self {
            Self::Schema => 2,
            _ => 1,
        }
    }
}

/// A Glue resource a tagging call named by ARN.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaggedResource {
    /// Which kind of resource the ARN's resource-type segment names.
    pub kind: TaggedResourceKind,
    /// Field 4 of the ARN: the Region whose estate the resource belongs to.
    pub region: String,
    /// Field 5 of the ARN: the account that owns the resource, which for a Data Catalog
    /// resource is also the `CatalogId` it lives under ("If none is provided, the Amazon
    /// Web Services account ID is used by default").
    pub account_id: String,
    /// The resource's name, folded exactly as the store keys it
    /// ([`TaggedResourceKind::folds_name`]), and for a schema set both of its names as the
    /// ARN carries them (`<registry>/<schema>`).
    pub name: String,
}

impl TaggedResource {
    /// The registry and the schema set a schema ARN names, or `None` for any other kind.
    fn schema_names(&self) -> Option<(&str, &str)> {
        if self.kind != TaggedResourceKind::Schema {
            return None;
        }
        self.name.split_once('/')
    }
}

/// The Glue resource `resource_arn` names, or `None` when it is not the ARN of a taggable
/// Glue resource at all — a value in another service's namespace, a malformed ARN, a Glue
/// resource kind that carries no tag set, or an ARN whose resource portion does not name
/// one resource of its kind.
///
/// Only the KIND and the identity fields are read here; whether this store holds the
/// resource is [`ComAmazonawsGlueAwsglueState::resource_tags`]'s answer, so an ARN of a
/// known kind that names nothing earns the tagging surface's not-found refusal rather than
/// being called malformed.
pub fn parse_tagged_resource(resource_arn: &str) -> Option<TaggedResource> {
    let parsed = parse_glue_arn(resource_arn)?;
    let (segment, name) = parsed.resource.split_once('/')?;
    let (_, kind) = TAGGED_RESOURCE_TYPES
        .iter()
        .find(|(candidate, _)| *candidate == segment)?;
    // The resource portion names exactly one resource of the kind: every part of the name
    // the kind carries, each non-empty, and nothing after them.
    let parts: Vec<&str> = name.split('/').collect();
    if parts.len() != kind.name_parts() || parts.iter().any(|part| part.is_empty()) {
        return None;
    }
    Some(TaggedResource {
        kind: *kind,
        region: parsed.region.clone(),
        account_id: parsed.account_id.clone(),
        name: if kind.folds_name() {
            fold_name(name)
        } else {
            name.to_string()
        },
    })
}

/// The message a refusal carries for a `ResourceArn` of a known kind that names no
/// resource this store holds, for the operations OUTSIDE the tagging surface that resolve
/// one ([`parse_tagged_resource`]).
///
/// Each sentence is the one this crate already answers wherever that kind fails to resolve
/// ([`messages`]), so "this ARN names no database" reads the same from an integration
/// property call as from `GetDatabase`. The kinds whose refusal wording no recording or doc
/// states take a sentence DERIVED from the request instead: it names the ARN that resolved
/// to nothing, and what is pinned is the behaviour — 400 with `EntityNotFoundException`.
///
/// The three TAGGING operations do not word their miss per kind: theirs is the recorded
/// fixed sentence ([`messages::TAGGED_RESOURCE_NOT_FOUND`]).
pub fn tagged_resource_not_found_message(resource: &TaggedResource, resource_arn: &str) -> String {
    match resource.kind {
        TaggedResourceKind::Database => messages::database_not_found(&resource.name),
        TaggedResourceKind::Crawler => messages::crawler_not_found(&resource.name),
        _ => format!("Entity not found for resource {resource_arn}"),
    }
}

/// Whether the refusal of a `ResourceArn` that is not the ARN of a taggable Glue resource
/// ([`parse_tagged_resource`] answers `None`) states any prose at all: it does NOT.
///
/// RECORDED behaviour of the tagging surface: such a request is answered with the
/// operation's modelled `InvalidInputException` carrying its CODE and no message member at
/// all — the refusal says that the ARN is not one of this service's, and states neither the
/// value it was handed nor the resource types the member admits. One copy, shared by the
/// three tagging operations that resolve a `ResourceArn` through [`parse_tagged_resource`],
/// so none of them invents prose for a refusal the service words silently.
pub const INVALID_RESOURCE_ARN_MESSAGE: Option<String> = None;

/// The message a refusal carries for a tag write that would leave the resource holding
/// more tags than one tag set may have ([`MAX_TAGS_PER_RESOURCE`]).
///
/// Derived from the request, like this crate's other refusals whose wording no recording
/// or doc states: it names how many tags the write would leave and the bound it passes.
/// What is pinned is the behaviour — 400 with `InvalidInputException`, and nothing
/// stored.
pub fn tag_limit_exceeded_message(would_hold: usize) -> String {
    format!(
        "TagsToAdd would leave the resource holding {would_hold} tags, more than the \
         {MAX_TAGS_PER_RESOURCE} allowed per resource"
    )
}

/// The ONE table from a resolved [`TaggedResource`] to the tag set of the record that holds
/// it, written once and expanded for the shared accessor and the mutable one alike — so a
/// kind is resolved in a single place and the read and the write of one ARN can never reach
/// two different records.
///
/// `get` is the map accessor (`get` or `get_mut`), `option` the one for a record held in an
/// `Option` (`as_ref` or `as_mut`), and the trailing tokens the borrow the accessor hands
/// back (`&` or `&mut`).
macro_rules! tag_set_of {
    ($store:expr, $resource:expr, $get:ident, $option:ident, $($borrow:tt)*) => {{
        let resource = $resource;
        // A resource that lives BESIDE the catalog tree is held in one per-account
        // namespace, so the ARN's account segment is what decides whether this emulator
        // could hold it at all. A database, a connection and a catalog resource live INSIDE
        // a catalog, so for them that segment is the `CatalogId` they are looked for under.
        let served = is_served_account(&resource.account_id);
        match resource.kind {
            TaggedResourceKind::Database => $store
                .catalogs
                .$get(&resource.account_id)
                .and_then(|catalog| catalog.databases.$get(&resource.name))
                .map(|database| $($borrow)* database.tags),
            TaggedResourceKind::Connection => $store
                .catalogs
                .$get(&resource.account_id)
                .and_then(|catalog| catalog.connections.$get(&resource.name))
                .map(|connection| $($borrow)* connection.tags),
            TaggedResourceKind::Catalog => $store
                .catalogs
                .$get(&child_catalog_id(&resource.account_id, &resource.name))
                .and_then(|catalog| catalog.definition.$option())
                .map(|definition| $($borrow)* definition.tags),
            _ if !served => None,
            TaggedResourceKind::Blueprint => $store
                .blueprints
                .$get(&resource.name)
                .map(|blueprint| $($borrow)* blueprint.tags),
            TaggedResourceKind::Crawler => $store
                .crawlers
                .$get(&resource.name)
                .map(|crawler| $($borrow)* crawler.tags),
            TaggedResourceKind::CustomEntityType => $store
                .custom_entity_types
                .$get(&resource.name)
                .map(|custom_entity_type| $($borrow)* custom_entity_type.tags),
            TaggedResourceKind::DataQualityRuleset => $store
                .data_quality_rulesets
                .$get(&resource.name)
                .map(|ruleset| $($borrow)* ruleset.tags),
            TaggedResourceKind::Job => $store
                .jobs
                .$get(&resource.region)
                .and_then(|jobs| jobs.$get(&resource.name))
                .map(|job| $($borrow)* job.tags),
            TaggedResourceKind::MlTransform => $store
                .ml_transforms
                .$get(&resource.name)
                .map(|transform| $($borrow)* transform.tags),
            TaggedResourceKind::Registry => $store
                .registries
                .$get(&resource.name)
                .map(|registry| $($borrow)* registry.tags),
            TaggedResourceKind::Schema => match resource.schema_names() {
                Some((registry_name, schema_name)) => $store
                    .registries
                    .$get(registry_name)
                    .and_then(|registry| registry.schemas.$get(schema_name))
                    .map(|schema| $($borrow)* schema.tags),
                None => None,
            },
            TaggedResourceKind::Session => $store
                .sessions
                .$get(&resource.region)
                .and_then(|sessions| sessions.$get(&resource.name))
                .map(|session| $($borrow)* session.tags),
            TaggedResourceKind::Trigger => $store
                .triggers
                .$get(&resource.region)
                .and_then(|triggers| triggers.$get(&resource.name))
                .map(|trigger| $($borrow)* trigger.tags),
            TaggedResourceKind::UsageProfile => $store
                .usage_profiles
                .$get(&resource.name)
                .map(|profile| $($borrow)* profile.tags),
            TaggedResourceKind::Workflow => $store
                .workflows
                .$get(&resource.name)
                .map(|workflow| $($borrow)* workflow.tags),
        }
    }};
}

impl ComAmazonawsGlueAwsglueState {
    /// The tag set of the resource `resource` names, or `None` when this store holds no
    /// such resource.
    ///
    /// The ONE resolution every tag READ goes through, so an ARN names the same record
    /// whichever operation — this service's own `GetTags`, or the account-wide tagging
    /// API that projects this store — resolved it.
    pub fn resource_tags(&self, resource: &TaggedResource) -> Option<&HashMap<String, String>> {
        tag_set_of!(self, resource, get, as_ref, &)
    }

    /// [`Self::resource_tags`] mutably: the ONE record every tag WRITE lands in, so a tag
    /// written through this service's own `TagResource` and one written through the
    /// account-wide tagging API are the same tag.
    pub fn resource_tags_mut(
        &mut self,
        resource: &TaggedResource,
    ) -> Option<&mut HashMap<String, String>> {
        tag_set_of!(self, resource, get_mut, as_mut, &mut)
    }

    /// Every resource of the account `account_id` this store holds a tag set for, as
    /// `(ARN, tags)`.
    ///
    /// This is ONE account's tag-store rows for this service: the tags a create recorded
    /// are the resource's own, so the account-wide tagging API projects THESE records
    /// rather than keeping a second copy that a create would have to be taught to write.
    /// Resources carrying NO tag are included — the caller decides what to do with an
    /// untagged resource — and `region` is the Region the ARNs are minted for, for the
    /// reason [`request_region`] gives.
    ///
    /// The account is the boundary the single-resource accessors enforce, so it bounds this
    /// projection too: the resources that live INSIDE a catalog are reached through the
    /// catalog of `account_id` alone, exactly as [`Self::resource_tags`] reaches them, and
    /// the kinds that live beside the catalog tree in one per-account namespace are
    /// projected only for an account this emulator serves ([`is_served_account`]), exactly
    /// as that accessor decides it. So no account's rows are ever reported to another's.
    pub fn tagged_resources(
        &self,
        account_id: &str,
        region: &str,
    ) -> Vec<(String, HashMap<String, String>)> {
        let mut found: Vec<(String, HashMap<String, String>)> = Vec::new();
        if let Some(catalog) = self.catalogs.get(account_id) {
            for database in catalog.databases.values() {
                push_row(
                    &mut found,
                    TaggedResourceKind::Database,
                    region,
                    &database.name,
                    &database.tags,
                );
            }
            for connection in catalog.connections.values() {
                push_row(
                    &mut found,
                    TaggedResourceKind::Connection,
                    region,
                    &connection.name,
                    &connection.tags,
                );
            }
        }
        for (catalog_id, catalog) in self.catalogs.iter() {
            if parse_catalog_reference(catalog_id).account_id != account_id {
                continue;
            }
            if let Some(definition) = &catalog.definition {
                push_row(
                    &mut found,
                    TaggedResourceKind::Catalog,
                    region,
                    &definition.name,
                    &definition.tags,
                );
            }
        }
        if !is_served_account(account_id) {
            return found;
        }
        for blueprint in self.blueprints.values() {
            push_row(
                &mut found,
                TaggedResourceKind::Blueprint,
                region,
                &blueprint.name,
                &blueprint.tags,
            );
        }
        for crawler in self.crawlers.values() {
            push_row(
                &mut found,
                TaggedResourceKind::Crawler,
                region,
                &crawler.name,
                &crawler.tags,
            );
        }
        for custom_entity_type in self.custom_entity_types.values() {
            push_row(
                &mut found,
                TaggedResourceKind::CustomEntityType,
                region,
                &custom_entity_type.name,
                &custom_entity_type.tags,
            );
        }
        for ruleset in self.data_quality_rulesets.values() {
            push_row(
                &mut found,
                TaggedResourceKind::DataQualityRuleset,
                region,
                &ruleset.name,
                &ruleset.tags,
            );
        }
        for job in self.jobs_in(region) {
            push_row(
                &mut found,
                TaggedResourceKind::Job,
                region,
                &job.name,
                &job.tags,
            );
        }
        for (transform_id, transform) in &self.ml_transforms {
            push_row(
                &mut found,
                TaggedResourceKind::MlTransform,
                region,
                transform_id,
                &transform.tags,
            );
        }
        for registry in self.registries.values() {
            push_row(
                &mut found,
                TaggedResourceKind::Registry,
                region,
                &registry.name,
                &registry.tags,
            );
            for schema in registry.schemas.values() {
                push_row(
                    &mut found,
                    TaggedResourceKind::Schema,
                    region,
                    &format!("{}/{}", registry.name, schema.name),
                    &schema.tags,
                );
            }
        }
        if let Some(sessions) = self.sessions.get(region) {
            for session in sessions.values() {
                push_row(
                    &mut found,
                    TaggedResourceKind::Session,
                    region,
                    &session.id,
                    &session.tags,
                );
            }
        }
        if let Some(triggers) = self.triggers.get(region) {
            for trigger in triggers.values() {
                push_row(
                    &mut found,
                    TaggedResourceKind::Trigger,
                    region,
                    &trigger.name,
                    &trigger.tags,
                );
            }
        }
        for profile in self.usage_profiles.values() {
            push_row(
                &mut found,
                TaggedResourceKind::UsageProfile,
                region,
                &profile.name,
                &profile.tags,
            );
        }
        for workflow in self.workflows.values() {
            push_row(
                &mut found,
                TaggedResourceKind::Workflow,
                region,
                &workflow.name,
                &workflow.tags,
            );
        }
        found
    }
}

/// One row of [`ComAmazonawsGlueAwsglueState::tagged_resources`]: the ARN of the resource
/// `name` of kind `kind` as served in `region`, with a copy of its tag set.
fn push_row(
    into: &mut Vec<(String, HashMap<String, String>)>,
    kind: TaggedResourceKind,
    region: &str,
    name: &str,
    tags: &HashMap<String, String>,
) {
    into.push((kind.arn(region, name), tags.clone()));
}
