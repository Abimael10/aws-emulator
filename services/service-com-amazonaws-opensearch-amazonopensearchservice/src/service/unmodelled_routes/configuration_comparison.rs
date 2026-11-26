// SPDX-License-Identifier: CC0-1.0

//! `CompareDomainConfiguration`: what differs between the configuration of two domains, per
//! category.
//!
//! Not in the on-disk model (see [`super`]), so its statuses, codes and messages are
//! RECORDED BEHAVIOUR ONLY. The checks run in the order the recordings show, and all of
//! them before either domain is resolved:
//!
//! 1. each side's ARN must BE a domain ARN — `400 ValidationException`, naming the form
//!    expected and the value as sent;
//! 2. every category named must be one of the supported set — `400 ValidationException`,
//!    naming the value as sent and the whole supported catalogue;
//! 3. only then is each side resolved, source first — `400 ValidationException`, naming
//!    which side and the domain it could not resolve.
//!
//! # What is compared
//!
//! The configuration this service HOLDS for each domain, read out of the two stored records
//! and nothing else: for each category, the configured entries of each side are collected
//! by the name that identifies them in that category, and the comparison counts the entries
//! only the source holds, the entries only the target holds, and the entries both hold with
//! different content. A domain compared against itself therefore counts zero on every
//! category, whatever it holds — which is the recorded answer for that call.

use std::collections::{BTreeMap, BTreeSet};

use ::serde_json::{json, Value};

use super::{required_text, Refusal};
use crate::state::{service_state, Domain};

/// The member each side of the comparison is named in, as a request spells it.
const SOURCE_DOMAIN_ARN: &str = "SourceDomainArn";
/// See [`SOURCE_DOMAIN_ARN`].
const TARGET_DOMAIN_ARN: &str = "TargetDomainArn";

/// The member the categories to compare are named in.
const CATEGORIES: &str = "Categories";

/// The categories this call compares, in the order the recorded refusal of an unsupported
/// one lists them — AWS's fixed catalogue, which is part of that message's fixed text.
///
/// Each name is a configuration surface a domain's cluster holds, and the entry below that
/// reads it is [`configured`].
const SUPPORTED_CATEGORIES: [&str; 11] = [
    "cluster_settings",
    "security_role_mappings",
    "ism_policies",
    "security_internal_users",
    "aliases",
    "alerting_monitors",
    "templates",
    "component_templates",
    "security_roles",
    "security_action_groups",
    "ingest_pipelines",
];

/// The form a domain ARN takes, as the recorded refusal of a value that is not one writes
/// it: a glob, in which the partition is `aws`-prefixed, the service, Region and account are
/// unconstrained, and the resource is a domain.
const DOMAIN_ARN_FORM: &str = "arn:aws*:*:*:*:domain/*";

/// The resource prefix of a domain ARN.
const DOMAIN_RESOURCE: &str = "domain/";

/// The status this call reports a category it compared under.
const COMPARED: &str = "compared";

/// What the comparison of `asked` answers.
pub(super) fn served<Eng: Send + Sync + 'static>(
    engine: &Eng,
    asked: &Value,
) -> Result<Value, Refusal> {
    let source_arn = required_text(asked, SOURCE_DOMAIN_ARN)?;
    let source_named = domain_named_by(&source_arn)
        .ok_or_else(|| Refusal::validation(invalid_domain_arn_message(&source_arn)))?
        .to_string();
    let target_arn = required_text(asked, TARGET_DOMAIN_ARN)?;
    let target_named = domain_named_by(&target_arn)
        .ok_or_else(|| Refusal::validation(invalid_domain_arn_message(&target_arn)))?
        .to_string();
    // Before any domain is resolved, and de-duplicated: a category named twice is one
    // category to compare, so what a request can ask for is bounded by the catalogue
    // itself rather than by the length of the list it sent.
    let categories = asked_categories(asked)?;
    let state = service_state(engine).ok_or_else(Refusal::unavailable)?;
    // A pure READ of both records: nothing about either domain changes because its
    // configuration was compared.
    let store = state.lock();
    let source = store
        .domain_of_arn(&source_arn)
        .ok_or_else(|| Refusal::validation(side_not_found_message(SOURCE, &source_named)))?;
    let target = store
        .domain_of_arn(&target_arn)
        .ok_or_else(|| Refusal::validation(side_not_found_message(TARGET, &target_named)))?;
    let compared: ::serde_json::Map<String, Value> = categories
        .iter()
        .map(|category| {
            (
                (*category).to_string(),
                difference(
                    &configured(source, category),
                    &configured(target, category),
                ),
            )
        })
        .collect();
    let mut document = ::serde_json::Map::new();
    document.insert(CATEGORIES.to_string(), Value::Object(compared));
    Ok(Value::Object(document))
}

/// How the refusal of an unresolvable side names it.
const SOURCE: &str = "Source";
/// See [`SOURCE`].
const TARGET: &str = "Target";

/// The categories the request asks for: the ones it named, or the whole catalogue when it
/// named none.
///
/// Every named category is checked against the catalogue BEFORE either domain is resolved,
/// and the first one outside it is what the request is refused for. A `Categories` that is
/// not a list is read as the one value it is, because whatever it is, it is not a supported
/// category.
fn asked_categories(asked: &Value) -> Result<Vec<&'static str>, Refusal> {
    let named: Vec<Value> = match asked.get(CATEGORIES) {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(elements)) => elements.clone(),
        Some(value) => vec![value.clone()],
    };
    if named.is_empty() {
        return Ok(SUPPORTED_CATEGORIES.to_vec());
    }
    let mut wanted: BTreeSet<&'static str> = BTreeSet::new();
    for element in &named {
        let sent = match element {
            Value::String(text) => text.clone(),
            // The value as SENT, which for anything but a string is its JSON text: a
            // caller is told back what it wrote.
            other => other.to_string(),
        };
        let supported = SUPPORTED_CATEGORIES
            .iter()
            .find(|category| **category == sent.as_str())
            .ok_or_else(|| Refusal::validation(unsupported_category_message(&sent)))?;
        wanted.insert(supported);
    }
    // In the catalogue's own order, so two requests naming the same categories are answered
    // alike whatever order they named them in.
    Ok(SUPPORTED_CATEGORIES
        .into_iter()
        .filter(|category| wanted.contains(category))
        .collect())
}

/// What this call says of a value that is not a domain ARN: the form it expected and the
/// value as sent.
///
/// The sentence ends with a full stop and a SPACE, which is part of the recorded fixed text.
fn invalid_domain_arn_message(sent: &str) -> String {
    format!("Invalid domain ARN format. Expected domain ARN ({DOMAIN_ARN_FORM}) but received: {sent}. ")
}

/// What it says of a category outside the supported set: the value as sent, and the whole
/// catalogue to choose from.
fn unsupported_category_message(sent: &str) -> String {
    format!(
        "Unsupported category: {sent}. Supported: [{}]",
        SUPPORTED_CATEGORIES.join(", ")
    )
}

/// What it says of a well-formed ARN naming a domain this account does not hold: which side
/// could not be resolved, and the domain its ARN named.
fn side_not_found_message(side: &str, domain_name: &str) -> String {
    format!("{side} domain {domain_name}: Domain {domain_name} not found.")
}

/// The domain a value NAMES, when that value is a domain ARN, and `None` when it is not one
/// at all.
///
/// The reading is [`DOMAIN_ARN_FORM`] exactly: six colon-separated fields, the first `arn`,
/// the second `aws`-prefixed, the next three unconstrained — a comparison takes an ARN of
/// any partition, service, Region and account, and resolves it afterwards — and the last a
/// domain resource naming something.
fn domain_named_by(arn: &str) -> Option<&str> {
    let mut fields = arn.splitn(6, ':');
    if fields.next()? != "arn" {
        return None;
    }
    if !fields.next()?.starts_with("aws") {
        return None;
    }
    // The service, the Region and the account, each a wildcard in the form above.
    let _ = fields.next()?;
    let _ = fields.next()?;
    let _ = fields.next()?;
    let named = fields.next()?.strip_prefix(DOMAIN_RESOURCE)?;
    (!named.is_empty()).then_some(named)
}

/// The configuration `domain` holds in `category`, keyed by what identifies an entry in that
/// category and carrying the content a comparison calls diverged when it differs.
///
/// A category this service holds no per-domain record for contributes no entry, which counts
/// as zero on both sides rather than as a difference.
fn configured(domain: &Domain, category: &str) -> BTreeMap<String, Value> {
    match category {
        // The cluster's own settings, keyed by the SCOPE and the setting's flat dotted name:
        // the two scopes hold the same name independently, so the scope is part of what
        // identifies a setting.
        "cluster_settings" => domain
            .cluster_settings
            .persistent
            .iter()
            .map(|(name, value)| (format!("{}/{name}", crate::state::SETTINGS_PERSISTENT), json!(value)))
            .chain(domain.cluster_settings.transient.iter().map(|(name, value)| {
                (format!("{}/{name}", crate::state::SETTINGS_TRANSIENT), json!(value))
            }))
            .collect(),
        // A role mapping is identified by the role it binds principals to, and its content
        // is the three member lists the security API reports.
        "security_role_mappings" => domain
            .security_role_mappings
            .iter()
            .map(|(role, mapping)| {
                (
                    role.clone(),
                    json!({
                        "backend_roles": mapping.backend_roles,
                        "users": mapping.users,
                        "hosts": mapping.hosts,
                    }),
                )
            })
            .collect(),
        "ism_policies" => domain
            .ism_policies
            .iter()
            .map(|(id, policy)| (id.clone(), policy.definition.clone()))
            .collect(),
        // An internal user is identified by its name. Its CREDENTIAL is no part of what is
        // compared: the domain holds it only to match a presented one against
        // ([`crate::state::fgac::Secret`]), and no read path reports it.
        "security_internal_users" => domain
            .internal_users
            .iter()
            .map(|(name, user)| {
                (
                    name.clone(),
                    json!({
                        "backend_roles": user.backend_roles,
                        "opendistro_security_roles": user.security_roles,
                        "attributes": user.attributes,
                    }),
                )
            })
            .collect(),
        // An alias is identified by its own name and names the indexes it stands for, which
        // is what differs between two domains holding one alias over different indexes.
        "aliases" => {
            let mut aliases: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
            for index in domain.indexes.values() {
                for alias in index.aliases.keys() {
                    aliases
                        .entry(alias.clone())
                        .or_default()
                        .insert(index.index_name.clone());
                }
            }
            aliases
                .into_iter()
                .map(|(alias, indexes)| (alias, json!(indexes)))
                .collect()
        }
        // A monitor is identified by its NAME rather than by the id the plugin minted for
        // it: two clusters configured alike hold the same monitor under two ids, and what is
        // being compared is the configuration. A monitor whose document names none is
        // identified by that id, which is all it has.
        "alerting_monitors" => domain
            .alerting
            .monitors
            .values()
            .map(|monitor| {
                let named = monitor
                    .definition
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or(&monitor.id);
                (named.to_string(), monitor.definition.clone())
            })
            .collect(),
        "templates" => domain
            .index_templates
            .iter()
            .map(|(name, template)| (name.clone(), template.definition.clone()))
            .collect(),
        "component_templates" => domain
            .component_templates
            .iter()
            .map(|(name, template)| (name.clone(), template.definition.clone()))
            .collect(),
        "security_roles" => domain
            .security_roles
            .iter()
            .map(|(name, role)| (name.clone(), role.definition.clone()))
            .collect(),
        // The action groups a domain's security plugin publishes, which every domain of this
        // service publishes alike — so two domains differ in this category only when the
        // published set itself differs.
        "security_action_groups" => crate::state::fgac_permissions::published_action_groups()
            .into_iter()
            .map(|(group, actions)| (group, json!(actions)))
            .collect(),
        "ingest_pipelines" => domain
            .ingest_pipelines
            .iter()
            .map(|(id, pipeline)| (id.clone(), pipeline.definition.clone()))
            .collect(),
        // Unreachable: every category reaching here is one of [`SUPPORTED_CATEGORIES`].
        _ => BTreeMap::new(),
    }
}

/// What one category's comparison reports: the status, and the three counts.
fn difference(source: &BTreeMap<String, Value>, target: &BTreeMap<String, Value>) -> Value {
    let source_only = source.keys().filter(|named| !target.contains_key(*named)).count();
    let target_only = target.keys().filter(|named| !source.contains_key(*named)).count();
    let diverged = source
        .iter()
        .filter(|(named, held)| target.get(*named).is_some_and(|other| other != *held))
        .count();
    json!({
        "Status": COMPARED,
        "Diverged": diverged,
        "SourceOnly": source_only,
        "TargetOnly": target_only,
    })
}
