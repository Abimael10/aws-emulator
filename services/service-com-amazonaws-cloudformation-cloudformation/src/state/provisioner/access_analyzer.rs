// SPDX-License-Identifier: CC0-1.0

//! The per-type provisioning arms of IAM Access Analyzer's own control plane: the analyzer an
//! `AWS::AccessAnalyzer::Analyzer` declares and the archive rule of one an
//! `AWS::AccessAnalyzer::ArchiveRule` declares. One file of [`super`]'s per-type arms,
//! dispatched to by its `Provisioner::AccessAnalyzerResource` variant, which carries which of
//! the two the declaration is ([`Resource`]).
//!
//! ## Why each resource is really created
//!
//! An ANALYZER is what that service monitors an account or an organization with:
//! `CreateAnalyzer` takes the name the declaration states, the analyzer's `type`, its
//! configuration, its inline archive rules and its tags, and answers the `arn` it recorded —
//! the value this type's `Ref` returns and the handle its tagging surface addresses one by.
//! Every later read of one (`GetAnalyzer`, `ListAnalyzers`) resolves it BY NAME, which is also
//! what `DeleteAnalyzer` takes, so the name is what each call here addresses.
//!
//! An ARCHIVE RULE is a resource OF an analyzer: `CreateArchiveRule` binds the analyzer as its
//! URI label and takes the rule's own name and the criteria it archives findings by,
//! `UpdateArchiveRule` restates those criteria for a rule that exists, and `GetArchiveRule`
//! reports the two instants that service keeps for it. A rule's name identifies it within one
//! analyzer, which is what [`super::super::template::name_scope_properties`] records for the
//! type.
//!
//! Both identities are computable from the template — an analyzer's ARN is built around the
//! name the declaration states, and a rule's around that ARN and the rule's own name — so
//! neither type is one [`super::super::template::identity_is_service_assigned`] names: what the
//! naming rules compute for one is the identity the resource really has. What those rules
//! CANNOT compute is the pair of instants a rule carries, which are that service's clock: they
//! reach the resource off its own report of the rule ([`reported_rule_attributes`]).
//!
//! ## The properties
//!
//! The analyzer's properties are `CreateAnalyzer`'s own members with a lower-cased initial
//! ([`camel_cased`]), with its `Tags` the `TagsMap` that call takes and its tags travelling
//! beside the ones CloudFormation gives the resource ([`tags_of`]). A rule's are
//! `CreateArchiveRule`'s the same way, except its FILTER, which the two declaring types spell
//! differently and [`filter_criteria`] reads into the one `FilterCriteriaMap` the API takes.
//!
//! A declared property no arm here carries is REFUSED rather than dropped
//! ([`refuse_unmapped_properties`]): a resource reported created with less than its declaration
//! asked for is the shallow success these arms exist to avoid.
//!
//! ## What a declaration that service refuses does
//!
//! Nothing here checks a declared value. An analyzer `Type` outside the set that service models,
//! a name outside `AnalyzerName`'s own pattern, a second account analyzer past the quota that
//! type publishes — each is refused BY IAM Access Analyzer, and that refusal is what fails the
//! resource and rolls the stack back, worded by the side that owns it.

use super::*;

/// Which of the two IAM Access Analyzer resources a declaration is, carried by the one
/// `Provisioner` variant these arms are reached through so the two share one set of arms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Resource {
    /// `AWS::AccessAnalyzer::Analyzer`.
    Analyzer,
    /// `AWS::AccessAnalyzer::ArchiveRule`.
    ArchiveRule,
}

/// The `AWS::<Namespace>::<Resource>` names of the two types, which the registry of
/// recognised types, the return-value catalogue and the primary-identifier table each key an
/// entry by. The middle segment is that model's own `sdkId` (`AccessAnalyzer`).
pub(in crate::state) const ACCESSANALYZER_ANALYZER: &str = "AWS::AccessAnalyzer::Analyzer";
pub(in crate::state) const ACCESSANALYZER_ARCHIVE_RULE: &str = "AWS::AccessAnalyzer::ArchiveRule";

/// IAM Access Analyzer's signing name, from its model (`sigv4Name: access-analyzer`), which is
/// also the ARN namespace its resources carry (`awsService.arnNamespace`). It speaks
/// `restJson1`, so its calls ride on [`call_rest_json_named`], each on the URI its own
/// operation's `@http` binding declares.
pub(in crate::state) const ACCESS_ANALYZER_SIGNING_NAME: &str = "access-analyzer";

/// The resource path an analyzer's ARN carries before its name
/// (`AnalyzerArn`: `^[^:]*:[^:]*:[^:]*:[^:]*:[^:]*:analyzer/.{1,255}$`), and the path a rule's
/// own identity carries inside that ARN.
const ANALYZER_ARN_PATH: &str = "analyzer";
const ARCHIVE_RULE_ARN_PATH: &str = "archive-rule";

/// The properties the two types name a resource by: the analyzer's own name, which both types
/// state — a rule states the analyzer it belongs to — and the rule's own.
pub(in crate::state) const ANALYZER_NAME_PROPERTY: &str = "AnalyzerName";
pub(in crate::state) const RULE_NAME_PROPERTY: &str = "RuleName";

/// The `Fn::GetAtt` attributes the two types publish: the ARN either resource is identified by,
/// and the two instants IAM Access Analyzer keeps for an archive rule.
pub(in crate::state) const ACCESS_ANALYZER_ARN_ATTRIBUTE: &str = "Arn";
pub(in crate::state) const ARCHIVE_RULE_CREATED_AT_ATTRIBUTE: &str = "CreatedAt";
pub(in crate::state) const ARCHIVE_RULE_UPDATED_AT_ATTRIBUTE: &str = "UpdatedAt";

/// Every attribute each type publishes, in the sorted order a resource's own attribute map
/// holds them in.
pub(in crate::state) const ANALYZER_ATTRIBUTES: &[&str] = &[ACCESS_ANALYZER_ARN_ATTRIBUTE];
pub(in crate::state) const ARCHIVE_RULE_ATTRIBUTES: &[&str] = &[
    ACCESS_ANALYZER_ARN_ATTRIBUTE,
    ARCHIVE_RULE_CREATED_AT_ATTRIBUTE,
    ARCHIVE_RULE_UPDATED_AT_ATTRIBUTE,
];

/// The operations these arms call, as IAM Access Analyzer's own model names them.
const CREATE_ANALYZER: &str = "CreateAnalyzer";
const UPDATE_ANALYZER: &str = "UpdateAnalyzer";
const DELETE_ANALYZER: &str = "DeleteAnalyzer";
const GET_ANALYZER: &str = "GetAnalyzer";
const CREATE_ARCHIVE_RULE: &str = "CreateArchiveRule";
const UPDATE_ARCHIVE_RULE: &str = "UpdateArchiveRule";
const DELETE_ARCHIVE_RULE: &str = "DeleteArchiveRule";
const GET_ARCHIVE_RULE: &str = "GetArchiveRule";
const TAG_RESOURCE: &str = "TagResource";
const UNTAG_RESOURCE: &str = "UntagResource";
const LIST_TAGS_FOR_RESOURCE: &str = "ListTagsForResource";

/// The collection an analyzer is created under (`PUT /analyzer`), which is also the path one
/// analyzer is addressed beneath, and the tagging collection (`/tags/{resourceArn}`).
const ANALYZER_URI: &str = "/analyzer";
const TAGS_URI: &str = "/tags";

/// The error code IAM Access Analyzer answers for a resource it does not hold
/// (`ResourceNotFoundException`, the modelled 404 of its reads, its updates and its deletes),
/// which is what says a delete has nothing left to remove.
const ACCESS_ANALYZER_NOT_FOUND: &str = "ResourceNotFoundException";

/// The members `CreateAnalyzer` takes the analyzer's own name and its type as.
const ANALYZER_NAME_MEMBER: &str = "analyzerName";
const ANALYZER_TYPE_MEMBER: &str = "type";

/// The property the analyzer's kind is declared in, the one its configuration is declared in
/// and the member both the create and the update take that configuration as.
const ANALYZER_TYPE_PROPERTY: &str = "Type";
const ANALYZER_CONFIGURATION_PROPERTY: &str = "AnalyzerConfiguration";
const CONFIGURATION_MEMBER: &str = "configuration";

/// The members `AnalyzerConfiguration`'s union takes the two configurations a declaration states
/// as: the type spells each with the `…Configuration` noun it is documented under, while that
/// union's own members are `unusedAccess` and `internalAccess`, so each declared alternative is
/// renamed onto the member that carries it ([`analyzer_configuration`]). Everything INSIDE one is
/// the member of the same name with a lower-cased initial, which is what [`camel_cased`] does.
const ANALYZER_CONFIGURATION_MEMBERS: &[(&str, &str)] = &[
    ("UnusedAccessConfiguration", "unusedAccess"),
    ("InternalAccessConfiguration", "internalAccess"),
];

/// The property an analyzer's declaration states its own archive rules in, and the member
/// `CreateAnalyzer` takes them as — "specifies the archive rules to add for the analyzer".
const ARCHIVE_RULES_PROPERTY: &str = "ArchiveRules";
const ARCHIVE_RULES_MEMBER: &str = "archiveRules";

/// The member a rule's own name travels as (`ruleName`, on the create, the update and the
/// read alike) and the member its criteria travel as.
const RULE_NAME_MEMBER: &str = "ruleName";
const FILTER_MEMBER: &str = "filter";

/// The property a rule's criteria are declared in, and the member an entry of the LIST
/// spelling of them names the filter key in.
const FILTER_PROPERTY: &str = "Filter";
const FILTER_KEY_MEMBER: &str = "Property";

/// The member `GetArchiveRule` answers the rule's record in, and the two instants that record
/// carries (`ArchiveRuleSummary$createdAt` / `$updatedAt`, both `@timestampFormat date-time`).
const ARCHIVE_RULE_MEMBER: &str = "archiveRule";
const CREATED_AT_MEMBER: &str = "createdAt";
const UPDATED_AT_MEMBER: &str = "updatedAt";

/// The member `GetAnalyzer` answers the analyzer's summary in, and the members that summary
/// carries the values this arm reads off it in.
const ANALYZER_MEMBER: &str = "analyzer";
const ARN_MEMBER: &str = "arn";
const NAME_MEMBER: &str = "name";
const TYPE_MEMBER: &str = "type";

/// The member a tag set travels as on the create and on `TagResource` alike: the `TagsMap` of
/// tag key to value both take ([`tags_as_map`]).
const TAGS_MEMBER: &str = "tags";

/// The query parameter `UntagResource` takes the keys to drop in (`tagKeys`, an `@httpQuery`
/// list, so one parameter per key).
const TAG_KEYS_QUERY: &str = "tagKeys";

/// Every property of the ANALYZER type these arms carry onto the owning API, so a declaration
/// stating one outside the set is REFUSED rather than reported as created without it.
const ANALYZER_PROPERTIES: &[&str] = &[
    ANALYZER_CONFIGURATION_PROPERTY,
    ANALYZER_NAME_PROPERTY,
    ARCHIVE_RULES_PROPERTY,
    "Tags",
    ANALYZER_TYPE_PROPERTY,
];

/// Every property of the ARCHIVE RULE type they carry, for the same reason. The type publishes
/// no tags at all — `CreateArchiveRule` takes none and that service holds no tags on a rule —
/// so none are propagated to one.
const ARCHIVE_RULE_PROPERTIES: &[&str] =
    &[ANALYZER_NAME_PROPERTY, FILTER_PROPERTY, RULE_NAME_PROPERTY];

/// The scalar properties `GetAnalyzer` reports back under the names the type declares, for
/// drift detection: the analyzer's own name and its kind.
///
/// Its tags are not among them — they are reconciled on their own
/// ([`retag_access_analyzer_resource`]) — and neither are the values that service alone
/// decides: the analyzer's status, the instant it was created and the resource it last
/// analyzed, none of which a declaration states.
const REPORTED_ANALYZER_PROPERTIES: &[(&str, &str)] = &[
    (ANALYZER_NAME_PROPERTY, NAME_MEMBER),
    (ANALYZER_TYPE_PROPERTY, TYPE_MEMBER),
];

/// Which of the two a resource type is, or `None` for a type that is neither — which is what
/// leaves every other type to the answer [`super::provisioner_for`] already gave it.
pub(super) fn access_analyzer_provisioner_for(resource_type: &str) -> Option<Resource> {
    match resource_type {
        ACCESSANALYZER_ANALYZER => Some(Resource::Analyzer),
        ACCESSANALYZER_ARCHIVE_RULE => Some(Resource::ArchiveRule),
        _ => None,
    }
}

/// The ARN of the analyzer `analyzer_name` in `region`, which is the form that service's own
/// `AnalyzerArn` carries: the one identity both this catalogue and IAM Access Analyzer compute
/// for an analyzer of that name, since both are built from the one account id this emulator
/// mints.
pub(in crate::state) fn analyzer_arn(region: &str, analyzer_name: &str) -> String {
    ::emulator_server::emulator_arn(
        ACCESS_ANALYZER_SIGNING_NAME,
        region,
        &format!("{ANALYZER_ARN_PATH}/{analyzer_name}"),
    )
}

/// The identity of the archive rule `rule_name` of that analyzer: the analyzer's own ARN with
/// the rule's path and name beneath it, a rule being a resource OF the analyzer rather than one
/// IAM Access Analyzer names on its own.
pub(in crate::state) fn archive_rule_arn(
    region: &str,
    analyzer_name: &str,
    rule_name: &str,
) -> String {
    format!(
        "{}/{ARCHIVE_RULE_ARN_PATH}/{rule_name}",
        analyzer_arn(region, analyzer_name)
    )
}

/// The analyzer a declaration names, as the NAME every call of that service on one takes.
///
/// A declaration states it as the analyzer's own name, and one that states it as a `Ref` of a
/// declared analyzer states the ARN that type's `Ref` returns — so an ARN is read down to the
/// name inside it and anything else is the name as written.
pub(in crate::state) fn declared_analyzer_name(declared: &str) -> &str {
    analyzer_name_in(declared)
}

/// Create the IAM Access Analyzer resource a declaration of one of the two types asks for, so a
/// client that read the ARN out of the stack's outputs finds the analyzer in that service's own
/// `GetAnalyzer`/`ListAnalyzers` and the rule in its `GetArchiveRule`/`ListArchiveRules`.
///
/// The ANALYZER's declaration IS the create's own members once their initials are lower-cased,
/// with the declared tags beside the ones CloudFormation gives the resource ([`tags_of`]) as the
/// `TagsMap` that call takes, and its inline archive rules as the `archiveRules` list it takes.
/// A RULE's are the create's members the same way, addressed at the analyzer its declaration
/// names, with its criteria read into the one map the API takes ([`filter_criteria`]); its two
/// instants are that service's clock, so they are read off its own report of the rule the create
/// just made.
pub(super) async fn create_access_analyzer_resource(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource_kind: Resource,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    refuse_unmapped_properties(resource, mapped_properties(resource_kind))?;
    let properties = resource
        .properties
        .as_object()
        .cloned()
        .unwrap_or_else(Map::new);
    match resource_kind {
        Resource::Analyzer => {
            let analyzer_name = analyzer_name_in(&resource.physical_resource_id).to_string();
            let mut request = Map::new();
            request.insert(
                ANALYZER_NAME_MEMBER.to_string(),
                Value::String(analyzer_name.clone()),
            );
            if let Some(kind) = properties.get(ANALYZER_TYPE_PROPERTY) {
                request.insert(ANALYZER_TYPE_MEMBER.to_string(), kind.clone());
            }
            if let Some(configuration) = properties.get(ANALYZER_CONFIGURATION_PROPERTY) {
                request.insert(
                    CONFIGURATION_MEMBER.to_string(),
                    analyzer_configuration(configuration)?,
                );
            }
            if let Some(rules) = properties.get(ARCHIVE_RULES_PROPERTY) {
                request.insert(ARCHIVE_RULES_MEMBER.to_string(), inline_rules(rules)?);
            }
            let tags = tags_as_map(&tags_of(&resource.properties, propagated));
            if !tags.is_empty() {
                request.insert(TAGS_MEMBER.to_string(), Value::Object(tags));
            }
            let answered = call_access_analyzer(
                ctx,
                region,
                CREATE_ANALYZER,
                "PUT",
                ANALYZER_URI,
                Some(&Value::Object(request)),
            )
            .await?;
            // The create answers the ARN that service recorded for the analyzer, which is the
            // identity this resource publishes; one it answered without is a create with no
            // handle for a client to call that service with, so the computed form is not
            // substituted for it.
            let arn = reported_text(&answered, ARN_MEMBER).unwrap_or_default();
            if arn.is_empty() {
                return Err(Refusal {
                    code: "InternalFailure".to_string(),
                    message: "the IAM Access Analyzer control plane answered a created \
                              analyzer without an ARN"
                        .to_string(),
                });
            }
            Ok(Created {
                physical_resource_id: Some(arn.clone()),
                attributes: arn_attribute(&arn),
            })
        }
        Resource::ArchiveRule => {
            let (analyzer_name, rule_name) = rule_identity(resource);
            let mut request = Map::new();
            request.insert(
                RULE_NAME_MEMBER.to_string(),
                Value::String(rule_name.clone()),
            );
            if let Some(filter) = properties.get(FILTER_PROPERTY) {
                request.insert(FILTER_MEMBER.to_string(), filter_criteria(filter)?);
            }
            call_access_analyzer(
                ctx,
                region,
                CREATE_ARCHIVE_RULE,
                "PUT",
                &archive_rules_uri(&analyzer_name),
                Some(&Value::Object(request)),
            )
            .await?;
            let arn = archive_rule_arn(region, &analyzer_name, &rule_name);
            let mut attributes = reported_rule_attributes(ctx, region, &analyzer_name, &rule_name)
                .await?
                .unwrap_or_default();
            attributes.insert(ACCESS_ANALYZER_ARN_ATTRIBUTE.to_string(), arn.clone());
            Ok(Created {
                physical_resource_id: Some(arn),
                attributes,
            })
        }
    }
}

/// Apply a changed declaration to the resource the stack already has, through the one call of
/// that service that changes it.
///
/// For an ANALYZER that is `UpdateAnalyzer`, which takes the analyzer's name together with the
/// configuration the template now declares — "this action is not supported for external access
/// analyzers", so a declaration restating the configuration of one is refused BY that service,
/// which is the answer a client making the same call gets — plus the reconciliation of the
/// analyzer's own inline archive rules against the ones it now declares. For a RULE it is
/// `UpdateArchiveRule`, which restates the criteria of a rule that exists and leaves the
/// instant it was created alone, bumping the one it was last updated at — both of which are read
/// back off that service's own report of the rule.
pub(super) async fn update_access_analyzer_resource(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource_kind: Resource,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Created, Refusal> {
    let properties = declared.as_object().cloned().unwrap_or_else(Map::new);
    if let Some(property) = unmapped_property(&properties, mapped_properties(resource_kind)) {
        return Err(Refusal {
            code: NOT_IMPLEMENTED.to_string(),
            message: format!(
                "this emulator does not apply the {property} property of {}",
                resource.resource_type
            ),
        });
    }
    match resource_kind {
        Resource::Analyzer => {
            let analyzer_name = analyzer_name_in(&resource.physical_resource_id).to_string();
            if let Some(configuration) = properties.get(ANALYZER_CONFIGURATION_PROPERTY) {
                let request = Value::Object(
                    [(
                        CONFIGURATION_MEMBER.to_string(),
                        analyzer_configuration(configuration)?,
                    )]
                    .into_iter()
                    .collect(),
                );
                call_access_analyzer(
                    ctx,
                    region,
                    UPDATE_ANALYZER,
                    "PUT",
                    &analyzer_uri(&analyzer_name),
                    Some(&request),
                )
                .await?;
            }
            reconcile_inline_rules(ctx, region, &analyzer_name, &resource.properties, declared)
                .await?;
            Ok(Created::attributes(arn_attribute(
                &resource.physical_resource_id,
            )))
        }
        Resource::ArchiveRule => {
            let (analyzer_name, rule_name) = rule_identity(resource);
            let mut request = Map::new();
            if let Some(filter) = properties.get(FILTER_PROPERTY) {
                request.insert(FILTER_MEMBER.to_string(), filter_criteria(filter)?);
            }
            call_access_analyzer(
                ctx,
                region,
                UPDATE_ARCHIVE_RULE,
                "PUT",
                &archive_rule_uri(&analyzer_name, &rule_name),
                Some(&Value::Object(request)),
            )
            .await?;
            let mut attributes = reported_rule_attributes(ctx, region, &analyzer_name, &rule_name)
                .await?
                .unwrap_or_default();
            attributes.insert(
                ACCESS_ANALYZER_ARN_ATTRIBUTE.to_string(),
                resource.physical_resource_id.clone(),
            );
            Ok(Created::attributes(attributes))
        }
    }
}

/// Bring the tags of the analyzer a resource record names to the ones this deployment gives it,
/// through that service's own `TagResource` and `UntagResource`.
///
/// `CreateAnalyzer`'s `tags` map applies to the analyzer that call creates and to no analyzer
/// that already exists, and `UpdateAnalyzer` carries no tags at all, so this is where the set an
/// analyzer is to end up carrying is reconciled: a key outside it is one this deployment no
/// longer gives the resource and is removed, and one in it with another value is re-valued.
/// Where the two already agree nothing is called, which is what makes this safe to ask on every
/// deployment; an analyzer that service no longer holds has no tags to reconcile.
///
/// Those calls address the resource by its ARN, which is this resource's physical id. An ARCHIVE
/// RULE has no tags at all — its type publishes none and that service holds none on a rule — so
/// nothing is asked about one.
pub(super) async fn retag_access_analyzer_resource(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource_kind: Resource,
    resource: &StoredResource,
    declared: &Value,
    propagated: &[(String, String)],
) -> Result<(), Refusal> {
    if resource_kind != Resource::Analyzer {
        return Ok(());
    }
    let arn = resource.physical_resource_id.clone();
    let answered = match call_access_analyzer(
        ctx,
        region,
        LIST_TAGS_FOR_RESOURCE,
        "GET",
        &tags_uri(&arn),
        None,
    )
    .await
    {
        Ok(answered) => answered,
        Err(refusal) if refusal.code == ACCESS_ANALYZER_NOT_FOUND => return Ok(()),
        Err(refusal) => return Err(refusal),
    };
    // That service answers a resource's tags as the MAP its own `TagsMap` is, which is one of
    // the two spellings a declaration's `Tags` property is written in, so one reader serves
    // both sides.
    let held = reported_tag_map(&answered, TAGS_MEMBER);
    let desired = tags_of(declared, propagated);
    let (added, removed) = tag_delta(&held, &desired);
    if !added.is_empty() {
        let request = Value::Object(
            [(TAGS_MEMBER.to_string(), Value::Object(tags_as_map(&added)))]
                .into_iter()
                .collect(),
        );
        call_access_analyzer(
            ctx,
            region,
            TAG_RESOURCE,
            "POST",
            &tags_uri(&arn),
            Some(&request),
        )
        .await?;
    }
    if !removed.is_empty() {
        call_access_analyzer(
            ctx,
            region,
            UNTAG_RESOURCE,
            "DELETE",
            &untag_uri(&arn, &removed),
            None,
        )
        .await?;
    }
    Ok(())
}

/// Delete the resource a stack created, by the identity it was created under — this resource's
/// physical id, which carries the analyzer's name and, for a rule, the rule's own name too.
///
/// One that service no longer holds (`ResourceNotFoundException`, judged on the CODE and never
/// on a message that carries the request's own values) is already in the state the delete asks
/// for.
pub(super) async fn delete_access_analyzer_resource(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource_kind: Resource,
    physical_resource_id: &str,
) -> Result<(), Refusal> {
    let (operation, uri) = match resource_kind {
        Resource::Analyzer => (
            DELETE_ANALYZER,
            analyzer_uri(analyzer_name_in(physical_resource_id)),
        ),
        Resource::ArchiveRule => (
            DELETE_ARCHIVE_RULE,
            archive_rule_uri(
                analyzer_name_in(physical_resource_id),
                rule_name_in(physical_resource_id),
            ),
        ),
    };
    match call_access_analyzer(ctx, region, operation, "DELETE", &uri, None).await {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == ACCESS_ANALYZER_NOT_FOUND => Ok(()),
        Err(refusal) => Err(refusal),
    }
}

/// The resource's configuration as IAM Access Analyzer reports it, for drift detection: that
/// service's own read of the analyzer by its name, or of the rule by the analyzer and the name
/// it was created under. One it no longer holds was deleted, which is what `None` reports.
///
/// A rule's FILTER is deliberately not among the properties reported for it: the two types that
/// declare criteria spell them differently ([`filter_criteria`]) while that service reports the
/// one criteria map either spelling became, so reporting that map against a declaration written
/// in the other spelling would be read as drift that is not there. What the read reports for a
/// rule is its own name and the analyzer it belongs to, and a property a read does not report is
/// not evidence of anything ([`super::super::drift`]).
pub(super) async fn describe_access_analyzer_resource(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource_kind: Resource,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    let analyzer_name = analyzer_name_in(physical_resource_id).to_string();
    match resource_kind {
        Resource::Analyzer => {
            let Some(summary) = read_analyzer(ctx, region, &analyzer_name).await? else {
                return Ok(None);
            };
            Ok(Some(reported_as_properties(
                &summary,
                REPORTED_ANALYZER_PROPERTIES,
            )))
        }
        Resource::ArchiveRule => {
            let rule_name = rule_name_in(physical_resource_id).to_string();
            let Some(rule) = read_archive_rule(ctx, region, &analyzer_name, &rule_name).await?
            else {
                return Ok(None);
            };
            let mut reported = Map::new();
            reported.insert(
                ANALYZER_NAME_PROPERTY.to_string(),
                Value::String(analyzer_name),
            );
            if let Some(name) = reported_text(&rule, RULE_NAME_MEMBER) {
                reported.insert(RULE_NAME_PROPERTY.to_string(), Value::String(name));
            }
            Ok(Some(Value::Object(reported)))
        }
    }
}

/// Every property of the type a declaration of `resource_kind` may state, which is every one
/// these arms carry onto the owning API.
fn mapped_properties(resource_kind: Resource) -> &'static [&'static str] {
    match resource_kind {
        Resource::Analyzer => ANALYZER_PROPERTIES,
        Resource::ArchiveRule => ARCHIVE_RULE_PROPERTIES,
    }
}

/// The analyzer and the rule one archive-rule resource addresses.
///
/// The rule's own name comes off the identity the resource carries. The ANALYZER's comes off the
/// declaration, which states it as that analyzer's name or as the ARN a `Ref` of a declared
/// analyzer resolves to — the resolved document, so a rule declared over another resource of the
/// same stack reaches the analyzer that resource really is. A declaration that states none at all
/// falls back to the identity, which carries the analyzer the rule was created under.
fn rule_identity(resource: &StoredResource) -> (String, String) {
    let declared = resource
        .properties
        .get(ANALYZER_NAME_PROPERTY)
        .and_then(Value::as_str)
        .map(analyzer_name_in)
        .filter(|analyzer_name| !analyzer_name.is_empty())
        .map(str::to_string);
    (
        declared.unwrap_or_else(|| analyzer_name_in(&resource.physical_resource_id).to_string()),
        rule_name_in(&resource.physical_resource_id).to_string(),
    )
}

/// The ANALYZER's name inside an identity of either type: the segment of the ARN after its
/// `analyzer/` path, up to the rule path a rule's identity carries. A value that is no such ARN
/// is already the name — which is how a declaration that states the analyzer by name reaches the
/// same answer as one that states it by `Ref`.
fn analyzer_name_in(identity: &str) -> &str {
    let after_path = match identity.split_once(&format!(":{ANALYZER_ARN_PATH}/")) {
        Some((_, after)) => after,
        None => return identity,
    };
    match after_path.split_once('/') {
        Some((name, _)) => name,
        None => after_path,
    }
}

/// The RULE's name inside an archive rule's identity: the segment after the rule path. An
/// identity carrying none is already the name.
fn rule_name_in(identity: &str) -> &str {
    match identity.split_once(&format!("/{ARCHIVE_RULE_ARN_PATH}/")) {
        Some((_, rule_name)) => rule_name,
        None => identity,
    }
}

/// The URI one analyzer is addressed at: the collection with the analyzer's name as the
/// `@httpLabel` of its read, its update and its delete, escaped as a single path segment
/// ([`percent_encoded`]) so a name outside the called model's own pattern reaches that
/// operation's validation as ONE value rather than as extra URI structure.
fn analyzer_uri(analyzer_name: &str) -> String {
    format!("{ANALYZER_URI}/{}", percent_encoded(analyzer_name))
}

/// The collection the archive rules of one analyzer live under
/// (`PUT /analyzer/{analyzerName}/archive-rule`).
fn archive_rules_uri(analyzer_name: &str) -> String {
    format!("{}/{ARCHIVE_RULE_ARN_PATH}", analyzer_uri(analyzer_name))
}

/// The URI one archive rule is addressed at, by the analyzer and the rule's own name.
fn archive_rule_uri(analyzer_name: &str, rule_name: &str) -> String {
    format!(
        "{}/{}",
        archive_rules_uri(analyzer_name),
        percent_encoded(rule_name)
    )
}

/// The URI the tagging calls address a resource at (`/tags/{resourceArn}`), with the ARN escaped
/// as a single path segment so its own `:` and `/` address one resource rather than another
/// path.
fn tags_uri(arn: &str) -> String {
    format!("{TAGS_URI}/{}", percent_encoded(arn))
}

/// That URI with the keys `UntagResource` is to drop as its `tagKeys` `@httpQuery` list — one
/// parameter per key, which is how a list bound to a query string travels.
fn untag_uri(arn: &str, keys: &[String]) -> String {
    let parameters = keys
        .iter()
        .map(|key| format!("{TAG_KEYS_QUERY}={}", percent_encoded(key)))
        .collect::<Vec<_>>()
        .join("&");
    format!("{}?{parameters}", tags_uri(arn))
}

/// The one attribute an analyzer publishes, and the one an archive rule publishes beside its
/// instants: the ARN the resource is identified by.
fn arn_attribute(arn: &str) -> BTreeMap<String, String> {
    [(ACCESS_ANALYZER_ARN_ATTRIBUTE.to_string(), arn.to_string())]
        .into_iter()
        .collect()
}

/// The two instants IAM Access Analyzer keeps for one archive rule, as the attributes the type
/// publishes them under — read off that service's own report of the rule, never from this
/// service's clock. A rule it reports nothing about contributes none.
async fn reported_rule_attributes(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    analyzer_name: &str,
    rule_name: &str,
) -> Result<Option<BTreeMap<String, String>>, Refusal> {
    let Some(rule) = read_archive_rule(ctx, region, analyzer_name, rule_name).await? else {
        return Ok(None);
    };
    let mut attributes = BTreeMap::new();
    for (attribute, member) in [
        (ARCHIVE_RULE_CREATED_AT_ATTRIBUTE, CREATED_AT_MEMBER),
        (ARCHIVE_RULE_UPDATED_AT_ATTRIBUTE, UPDATED_AT_MEMBER),
    ] {
        if let Some(instant) = rule.get(member).and_then(instant_text) {
            attributes.insert(attribute.to_string(), instant_to_the_second(&instant));
        }
    }
    Ok(Some(attributes))
}

/// An instant that service reported, as the text this type's return value carries: the ISO-8601
/// UTC instant to the SECOND.
///
/// NOT [`super::super::provisioning::published_instant`], the millisecond rendering most of this
/// catalogue's instants are published in: the two values THIS type publishes are the instants IAM
/// Access Analyzer records for a rule, and an archive rule's are whole seconds — its own record
/// carries no finer part — so what the resource publishes is that instant rather than a writer's
/// precision. `ArchiveRuleSummary`'s `createdAt` and `updatedAt` are `@timestampFormat date-time`
/// members, which a serializer may write with whatever sub-second digits the clock behind it had,
/// so the fraction is dropped rather than copied digit for digit.
///
/// TRUNCATED, never rounded: a published instant is never later than the one it reports. Text
/// this does not recognise as a UTC instant — one carrying an offset rather than `Z`, or no time
/// at all — is passed through exactly as that service wrote it.
fn instant_to_the_second(reported: &str) -> String {
    let Some(body) = reported.strip_suffix('Z') else {
        return reported.to_string();
    };
    if !body.contains('T') {
        return reported.to_string();
    }
    match body.split_once('.') {
        Some((seconds, fraction)) if fraction.chars().all(|digit| digit.is_ascii_digit()) => {
            format!("{seconds}Z")
        }
        Some(_) => reported.to_string(),
        None => reported.to_string(),
    }
}

/// The analyzer summary that service holds under `analyzer_name`, or `None` when it holds none
/// (`ResourceNotFoundException`, judged on the CODE).
async fn read_analyzer(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    analyzer_name: &str,
) -> Result<Option<Value>, Refusal> {
    match call_access_analyzer(
        ctx,
        region,
        GET_ANALYZER,
        "GET",
        &analyzer_uri(analyzer_name),
        None,
    )
    .await
    {
        Ok(answered) => Ok(answered.get(ANALYZER_MEMBER).cloned()),
        Err(refusal) if refusal.code == ACCESS_ANALYZER_NOT_FOUND => Ok(None),
        Err(refusal) => Err(refusal),
    }
}

/// The archive rule record that service holds for one analyzer, or `None` when it holds none.
async fn read_archive_rule(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    analyzer_name: &str,
    rule_name: &str,
) -> Result<Option<Value>, Refusal> {
    match call_access_analyzer(
        ctx,
        region,
        GET_ARCHIVE_RULE,
        "GET",
        &archive_rule_uri(analyzer_name, rule_name),
        None,
    )
    .await
    {
        Ok(answered) => Ok(answered.get(ARCHIVE_RULE_MEMBER).cloned()),
        Err(refusal) if refusal.code == ACCESS_ANALYZER_NOT_FOUND => Ok(None),
        Err(refusal) => Err(refusal),
    }
}

/// Bring the archive rules an analyzer's own declaration states to the ones it now declares: a
/// rule the new declaration no longer names is deleted, one it names with other criteria is
/// restated through `UpdateArchiveRule`, and one it names for the first time is created.
///
/// `CreateAnalyzer`'s `archiveRules` member applies to the analyzer that call creates and to no
/// analyzer that already exists, so this is the only way a changed inline rule set reaches that
/// service — and a rule declared on the analyzer is one of its resources exactly as a rule
/// declared as an `AWS::AccessAnalyzer::ArchiveRule` of its own is.
async fn reconcile_inline_rules(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    analyzer_name: &str,
    deployed: &Value,
    declared: &Value,
) -> Result<(), Refusal> {
    let held = inline_rules_of(deployed);
    let desired = inline_rules_of(declared);
    for (rule_name, _) in &held {
        if !desired.iter().any(|(name, _)| name == rule_name) {
            call_access_analyzer(
                ctx,
                region,
                DELETE_ARCHIVE_RULE,
                "DELETE",
                &archive_rule_uri(analyzer_name, rule_name),
                None,
            )
            .await?;
        }
    }
    for (rule_name, filter) in &desired {
        let request = Value::Object(
            [(FILTER_MEMBER.to_string(), filter_criteria(filter)?)]
                .into_iter()
                .collect(),
        );
        match held.iter().find(|(name, _)| name == rule_name) {
            // A rule that service already holds under the same criteria needs nothing asked of
            // it; one it holds under others is restated by the call that changes a rule.
            Some((_, held_filter)) => {
                if held_filter != filter {
                    call_access_analyzer(
                        ctx,
                        region,
                        UPDATE_ARCHIVE_RULE,
                        "PUT",
                        &archive_rule_uri(analyzer_name, rule_name),
                        Some(&request),
                    )
                    .await?;
                }
            }
            None => {
                let mut created = request.as_object().cloned().unwrap_or_else(Map::new);
                created.insert(
                    RULE_NAME_MEMBER.to_string(),
                    Value::String(rule_name.clone()),
                );
                call_access_analyzer(
                    ctx,
                    region,
                    CREATE_ARCHIVE_RULE,
                    "PUT",
                    &archive_rules_uri(analyzer_name),
                    Some(&Value::Object(created)),
                )
                .await?;
            }
        }
    }
    Ok(())
}

/// The inline archive rules one analyzer declaration states, as the pairs of rule name and
/// declared criteria they are. A declaration that states none states no rule, and an entry
/// naming no rule is not a rule at all.
fn inline_rules_of(properties: &Value) -> Vec<(String, Value)> {
    properties
        .get(ARCHIVE_RULES_PROPERTY)
        .and_then(Value::as_array)
        .map(|rules| {
            rules
                .iter()
                .filter_map(|rule| {
                    let name = rule.get(RULE_NAME_PROPERTY)?.as_str()?;
                    Some((
                        name.to_string(),
                        rule.get(FILTER_PROPERTY).cloned().unwrap_or(Value::Null),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The inline archive rules of an analyzer's declaration as the `archiveRules` list
/// `CreateAnalyzer` takes: each entry's `RuleName` under the member of the same name with a
/// lower-cased initial, and its criteria read into the one map the API takes.
fn inline_rules(declared: &Value) -> Result<Value, Refusal> {
    let Some(rules) = declared.as_array() else {
        // A declaration that is not a list states no rule of this form; the owning operation's
        // own validation is what refuses it rather than this dropping it.
        return Ok(declared.clone());
    };
    let mut converted = Vec::with_capacity(rules.len());
    for rule in rules {
        let mut entry = Map::new();
        if let Some(name) = rule.get(RULE_NAME_PROPERTY) {
            entry.insert(RULE_NAME_MEMBER.to_string(), name.clone());
        }
        if let Some(filter) = rule.get(FILTER_PROPERTY) {
            entry.insert(FILTER_MEMBER.to_string(), filter_criteria(filter)?);
        }
        converted.push(Value::Object(entry));
    }
    Ok(Value::Array(converted))
}

/// One declared `AnalyzerConfiguration` as the union member `CreateAnalyzer` and
/// `UpdateAnalyzer` both take it as.
///
/// The type spells each alternative with the `…Configuration` noun it is documented under while
/// the union's own members are `unusedAccess` and `internalAccess`
/// ([`ANALYZER_CONFIGURATION_MEMBERS`]), so a declared alternative is renamed onto the member
/// that carries it, with everything inside it re-cased. A declaration already written as those
/// members, or one naming neither alternative, is re-cased and passed through, so the owning
/// operation's own validation is what refuses it rather than this dropping it.
fn analyzer_configuration(declared: &Value) -> Result<Value, Refusal> {
    let Some(members) = declared.as_object() else {
        return Ok(declared.clone());
    };
    let mut converted = Map::new();
    for (property, member) in ANALYZER_CONFIGURATION_MEMBERS {
        if let Some(configuration) = members.get(*property) {
            converted.insert((*member).to_string(), camel_cased(configuration, 0)?);
        }
    }
    if converted.is_empty() {
        return camel_cased(declared, 0);
    }
    Ok(Value::Object(converted))
}

/// One declared filter as the `FilterCriteriaMap` both creates and the update take: a map from
/// one of that service's own filter KEYS (`resourceType`, `principal.AWS`) to the criterion that
/// narrows it, whose members are `eq`, `neq`, `contains` and `exists`.
///
/// The two declaring types spell the same criteria differently, and both are read here:
///
/// * as that MAP, which is what an `AWS::AccessAnalyzer::ArchiveRule` states — the filter keys
///   are the author's own, case-sensitive, so they travel verbatim and only each criterion's
///   members are re-cased ([`camel_cased`] lower-cases each initial, so `Eq` is `eq`);
/// * as a LIST of entries each naming its filter key in a `Property` member, which is what an
///   analyzer's own inline `ArchiveRules` state — so each entry becomes the one map entry its
///   `Property` names, with the operators beside it as that key's criterion.
///
/// A declaration that is neither is passed through as it came, so the owning operation's own
/// validation refuses it rather than this silently dropping it.
fn filter_criteria(declared: &Value) -> Result<Value, Refusal> {
    match declared {
        Value::Object(criteria) => {
            let mut converted = Map::new();
            for (filter_key, criterion) in criteria {
                converted.insert(filter_key.clone(), camel_cased(criterion, 0)?);
            }
            Ok(Value::Object(converted))
        }
        Value::Array(entries) => {
            let mut converted = Map::new();
            for entry in entries {
                let Some(members) = entry.as_object() else {
                    return Ok(declared.clone());
                };
                let Some(filter_key) = members.get(FILTER_KEY_MEMBER).and_then(Value::as_str)
                else {
                    return Ok(declared.clone());
                };
                let criterion: Map<String, Value> = members
                    .iter()
                    .filter(|(member, _)| member.as_str() != FILTER_KEY_MEMBER)
                    .map(|(member, value)| (lower_initial(member), value.clone()))
                    .collect();
                converted.insert(filter_key.to_string(), Value::Object(criterion));
            }
            Ok(Value::Object(converted))
        }
        other => Ok(other.clone()),
    }
}

/// Call one IAM Access Analyzer operation on the `restJson1` wire its model declares, on the
/// method and URI that operation's own `@http` binding names — naming the operation so a
/// refusal is reported the way that service's own SDK raises it.
async fn call_access_analyzer(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    operation: &str,
    method: &'static str,
    uri: &str,
    body: Option<&Value>,
) -> Result<Value, Refusal> {
    call_rest_json_named(
        ctx,
        ACCESS_ANALYZER_SIGNING_NAME,
        region,
        operation,
        method,
        uri,
        body,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: an analyzer's identity is the ARN built around the name the declaration states, and
    /// a rule's is that ARN with the rule's own name beneath it — and either identity is read
    /// back down to the names every call of that service takes.
    ///
    /// The rows differ along the analyzer name's form (a plain name, one carrying the
    /// punctuation `AnalyzerName`'s own pattern admits, one beginning with the underscore that
    /// pattern allows) and along the rule's.
    #[test]
    fn an_identity_round_trips_to_the_names_the_api_takes() {
        let rows: &[(&str, &str, &str)] = &[
            ("a plain name", "audit", "ArchiveS3"),
            (
                "names carrying the punctuation the pattern admits",
                "org.audit-2026_q1",
                "Archive.s3-buckets_2026",
            ),
            ("a name beginning with an underscore", "_internal", "Rule1"),
        ];
        for (case, analyzer_name, rule_name) in rows {
            let analyzer = analyzer_arn("eu-west-1", analyzer_name);
            assert_eq!(
                analyzer_name_in(&analyzer),
                *analyzer_name,
                "case {case:?}: expected the analyzer name back out of {analyzer:?}, got {:?}",
                analyzer_name_in(&analyzer)
            );
            let rule = archive_rule_arn("eu-west-1", analyzer_name, rule_name);
            assert_eq!(
                rule,
                format!("{analyzer}/{ARCHIVE_RULE_ARN_PATH}/{rule_name}"),
                "case {case:?}: expected the rule's identity to nest inside the analyzer's, got \
                 {rule:?}"
            );
            assert_eq!(
                analyzer_name_in(&rule),
                *analyzer_name,
                "case {case:?}: expected the analyzer name out of {rule:?}, got {:?}",
                analyzer_name_in(&rule)
            );
            assert_eq!(
                rule_name_in(&rule),
                *rule_name,
                "case {case:?}: expected the rule name out of {rule:?}, got {:?}",
                rule_name_in(&rule)
            );
            // A declaration that states the analyzer by NAME rather than by `Ref` reaches the
            // same answer.
            assert_eq!(
                declared_analyzer_name(analyzer_name),
                *analyzer_name,
                "case {case:?}: expected a declared name to be read as itself"
            );
        }
    }

    /// RULE: a declared filter becomes the criteria map the API takes, whichever of the two
    /// spellings the declaring type uses — the filter keys verbatim, each criterion's own
    /// members re-cased.
    #[test]
    fn either_spelling_of_a_filter_becomes_the_criteria_map() {
        let rows: &[(&str, Value, Value)] = &[
            (
                "the map spelling, with an upper-cased operator",
                ::serde_json::json!({"resourceType": {"Eq": ["AWS::S3::Bucket"]}}),
                ::serde_json::json!({"resourceType": {"eq": ["AWS::S3::Bucket"]}}),
            ),
            (
                "the map spelling, already written as the API members",
                ::serde_json::json!({"principal.AWS": {"contains": ["123456789012"]}}),
                ::serde_json::json!({"principal.AWS": {"contains": ["123456789012"]}}),
            ),
            (
                "two filter keys, one of them an existence check",
                ::serde_json::json!({
                    "resourceType": {"Neq": ["AWS::SQS::Queue"]},
                    "condition.aws:PrincipalOrgID": {"Exists": true},
                }),
                ::serde_json::json!({
                    "resourceType": {"neq": ["AWS::SQS::Queue"]},
                    "condition.aws:PrincipalOrgID": {"exists": true},
                }),
            ),
            (
                "the list spelling an analyzer's inline rules use",
                ::serde_json::json!([
                    {"Property": "resourceType", "Eq": ["AWS::S3::Bucket"]},
                    {"Property": "isPublic", "Exists": true},
                ]),
                ::serde_json::json!({
                    "resourceType": {"eq": ["AWS::S3::Bucket"]},
                    "isPublic": {"exists": true},
                }),
            ),
        ];
        for (case, declared, expected) in rows {
            let criteria = filter_criteria(declared).expect("a filter inside the nesting bound");
            assert_eq!(
                criteria, *expected,
                "case {case:?}: expected {expected}, got {criteria}"
            );
        }
    }

    /// RULE: an instant that service reported is published as the ISO-8601 UTC instant to the
    /// second, whatever sub-second digits the text it came in carried.
    ///
    /// The rows differ along how much fraction the reported text carries (none, three digits,
    /// six), and on text that is no UTC instant at all, which is passed through.
    #[test]
    fn a_published_instant_is_the_instant_to_the_second() {
        let rows: &[(&str, &str, &str)] = &[
            (
                "no fraction at all",
                "2026-01-02T03:04:05Z",
                "2026-01-02T03:04:05Z",
            ),
            (
                "milliseconds",
                "2026-01-02T03:04:05.678Z",
                "2026-01-02T03:04:05Z",
            ),
            (
                "microseconds, truncated rather than rounded",
                "2026-01-02T03:04:05.999999Z",
                "2026-01-02T03:04:05Z",
            ),
            (
                "an instant written with an offset rather than Z",
                "2026-01-02T03:04:05+00:00",
                "2026-01-02T03:04:05+00:00",
            ),
            ("no time at all", "2026-01-02", "2026-01-02"),
        ];
        for (case, reported, expected) in rows {
            let published = instant_to_the_second(reported);
            assert_eq!(
                published, *expected,
                "case {case:?}: expected {expected:?}, got {published:?}"
            );
        }
    }

    /// RULE: a declared `AnalyzerConfiguration` becomes the union member the API takes, each
    /// alternative renamed off the `…Configuration` noun the type spells it with.
    #[test]
    fn a_declared_configuration_becomes_the_union_member_it_names() {
        let rows: &[(&str, Value, Value)] = &[
            (
                "the unused-access alternative",
                ::serde_json::json!({"UnusedAccessConfiguration": {"UnusedAccessAge": 90}}),
                ::serde_json::json!({"unusedAccess": {"unusedAccessAge": 90}}),
            ),
            (
                "the internal-access alternative",
                ::serde_json::json!({
                    "InternalAccessConfiguration": {"InternalAccessAnalysisRule": {}}
                }),
                ::serde_json::json!({"internalAccess": {"internalAccessAnalysisRule": {}}}),
            ),
            (
                "a declaration already written as the union's own member",
                ::serde_json::json!({"unusedAccess": {"unusedAccessAge": 1}}),
                ::serde_json::json!({"unusedAccess": {"unusedAccessAge": 1}}),
            ),
        ];
        for (case, declared, expected) in rows {
            let configuration =
                analyzer_configuration(declared).expect("a configuration inside the bound");
            assert_eq!(
                configuration, *expected,
                "case {case:?}: expected {expected}, got {configuration}"
            );
        }
    }

    /// Every property each type publishes is one an arm here carries, and every member the
    /// creates take is a property the type publishes: the two catalogues cannot drift apart,
    /// which is what keeps a declaration from being reported as applied while a member went
    /// nowhere.
    #[test]
    fn every_carried_member_is_a_published_property() {
        for property in [
            ANALYZER_NAME_PROPERTY,
            ANALYZER_TYPE_PROPERTY,
            ANALYZER_CONFIGURATION_PROPERTY,
            ARCHIVE_RULES_PROPERTY,
        ] {
            assert!(
                ANALYZER_PROPERTIES.contains(&property),
                "case {property:?}: expected a member of the analyzer's create to be a property \
                 this arm applies, it is not in {ANALYZER_PROPERTIES:?}"
            );
        }
        for property in [ANALYZER_NAME_PROPERTY, RULE_NAME_PROPERTY, FILTER_PROPERTY] {
            assert!(
                ARCHIVE_RULE_PROPERTIES.contains(&property),
                "case {property:?}: expected a member of the rule's create to be a property this \
                 arm applies, it is not in {ARCHIVE_RULE_PROPERTIES:?}"
            );
        }
        for (property, _) in REPORTED_ANALYZER_PROPERTIES {
            assert!(
                ANALYZER_PROPERTIES.contains(property),
                "case {property:?}: expected a reported property to be one the type publishes, it \
                 is not in {ANALYZER_PROPERTIES:?}"
            );
        }
    }
}
