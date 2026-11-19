// SPDX-License-Identifier: CC0-1.0

//! The per-type provisioning arms of the two LAKE FORMATION PERMISSION types a template
//! grants Data Catalog access with: the legacy `AWS::LakeFormation::Permissions` and the
//! newer `AWS::LakeFormation::PrincipalPermissions`. One file of [`super`]'s per-type arms,
//! dispatched to by its `Provisioner`.
//!
//! A resource of either type IS one grant: Lake Formation's `GrantPermissions` records the
//! permissions a principal holds on a Data Catalog resource and `RevokePermissions` takes
//! them away again (`smithy_query(query="operation",
//! operation="GrantPermissions,RevokePermissions,ListPermissions")` of the `lakeformation`
//! model: `POST /GrantPermissions`, `POST /RevokePermissions`, `POST /ListPermissions`, each
//! taking the `Principal`, the `Resource` and the two permission lists). So the create is
//! that grant, the delete is the matching revoke, and a declaration whose permission lists
//! CHANGED is the revoke of what the grant no longer carries plus the grant of what it adds
//! — the same pair of calls a client makes.
//!
//! Both types describe the same grant, and differ only in how a template SPELLS it:
//!
//! * `AWS::LakeFormation::PrincipalPermissions` states `Principal`, `Resource`,
//!   `Permissions` and `PermissionsWithGrantOption` under the names Lake Formation's own
//!   call takes them, plus the `Catalog` that call takes as its `CatalogId` — so the
//!   evaluated property document IS the request;
//! * `AWS::LakeFormation::Permissions` states the principal as `DataLakePrincipal` and names
//!   each member of its `Resource` with a `…Resource` suffix ([`LEGACY_RESOURCE_MEMBERS`]),
//!   with a data location's ARN spelled `S3Resource` where that service's own member is
//!   `ResourceArn` — so its arm renames those members and sends the same request.
//!
//! Neither type publishes a `Tags` property and neither call takes one: a grant is not a
//! taggable resource — Lake Formation holds tags on the LF-tags its own tagging calls name —
//! so no tags are propagated to one.
//!
//! ## What identifies one
//!
//! A grant declares no name: what identifies it is the PAIR it binds, which is what each
//! type's `Ref` returns ([`crate::state::template::return_values`] computes both from the
//! resolved declaration):
//!
//! * the newer type's two read-only properties are the two halves of its composite primary
//!   identifier — `PrincipalIdentifier` is the principal as its own structure
//!   ([`principal_identifier`]) and `ResourceIdentifier` the resource as its own
//!   ([`resource_identifier`]) — so the resource's physical id is those two halves joined;
//!   there is nothing else to identify a grant by, and both are values the type's handler
//!   computes rather than inputs it takes
//!   ([`crate::state::template::read_only_properties`]).
//! * the legacy type's id is the principal and the ARN of the Data Catalog resource the
//!   grant is on ([`legacy_identity`]).
//!
//! Because the identity IS that pair, a declaration that re-points either side names a
//! DIFFERENT grant: the deployment creates it and revokes the one it replaced
//! ([`super::provisioning::is_same_resource`]) rather than reaching the update arm.

use super::*;

/// `AWS::LakeFormation::Permissions`: the legacy type, whose `Ref` returns the identifier of
/// the grant it describes.
pub(in crate::state) const LAKEFORMATION_PERMISSIONS: &str = "AWS::LakeFormation::Permissions";

/// `AWS::LakeFormation::PrincipalPermissions`: the newer type, whose `Ref` returns its
/// composite primary identifier and whose two read-only properties are its halves.
pub(in crate::state) const LAKEFORMATION_PRINCIPAL_PERMISSIONS: &str =
    "AWS::LakeFormation::PrincipalPermissions";

/// Which of the two types one grant is declared with: the spelling of the declaration is all
/// that differs between them, so one set of arms serves both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::state) enum Grant {
    /// `AWS::LakeFormation::Permissions`.
    Legacy,
    /// `AWS::LakeFormation::PrincipalPermissions`.
    Principal,
}

/// Lake Formation's signing name, from its model (`sigv4Name`/`endpointPrefix`
/// `lakeformation`), which speaks `restJson1` — so its calls ride on [`call_rest_json_named`],
/// each on the URI its own operation's `@http` binding declares.
const LAKEFORMATION_SIGNING_NAME: &str = "lakeformation";

/// The three operations this module calls, as that service's own model names and binds them.
const GRANT_PERMISSIONS: &str = "GrantPermissions";
const REVOKE_PERMISSIONS: &str = "RevokePermissions";
const LIST_PERMISSIONS: &str = "ListPermissions";
const GRANT_PERMISSIONS_URI: &str = "/GrantPermissions";
const REVOKE_PERMISSIONS_URI: &str = "/RevokePermissions";
const LIST_PERMISSIONS_URI: &str = "/ListPermissions";

/// The members `GrantPermissions` and `RevokePermissions` take, which both types state (the
/// newer one under exactly these names).
const PRINCIPAL_MEMBER: &str = "Principal";
const RESOURCE_MEMBER: &str = "Resource";
const PERMISSIONS_MEMBER: &str = "Permissions";
const PERMISSIONS_WITH_GRANT_OPTION_MEMBER: &str = "PermissionsWithGrantOption";
const CATALOG_ID_MEMBER: &str = "CatalogId";

/// The member that carries the principal's identifier inside `DataLakePrincipal`
/// ("an identifier for the Lake Formation principal").
const DATA_LAKE_PRINCIPAL_IDENTIFIER_MEMBER: &str = "DataLakePrincipalIdentifier";

/// The newer type's property that names the Data Catalog the grant is made in, which is the
/// `CatalogId` the calls take ("the identifier for the Data Catalog. By default, the account
/// ID").
const CATALOG_PROPERTY: &str = "Catalog";

/// The two properties the newer type's schema marks READ-ONLY, which are the two halves of
/// its composite primary identifier and the two `Fn::GetAtt` attributes it publishes: no
/// handler of the type takes either as an input, and both are computed from the grant the
/// declaration describes ([`principal_identifier`], [`resource_identifier`]).
pub(in crate::state) const PRINCIPAL_IDENTIFIER_PROPERTY: &str = "PrincipalIdentifier";
pub(in crate::state) const RESOURCE_IDENTIFIER_PROPERTY: &str = "ResourceIdentifier";

/// The legacy type's property that carries the principal, which the calls take as
/// `Principal`.
const DATA_LAKE_PRINCIPAL_PROPERTY: &str = "DataLakePrincipal";

/// The members of Lake Formation's `Resource` the two types publish, in the order each type's
/// own property reference lists them — the newer type's `Resource` property, and the legacy
/// type's under its own `…Resource` spelling.
///
/// Each is a member of that service's `Resource` shape
/// (`smithy_query(query="shape", shape="Resource")`); the two members that shape carries
/// BEYOND them (`LFTagExpression` and `RLSPolicy`) are no property of either type, so a
/// declaration states no resource of those kinds and the projection publishes no member for
/// them.
pub(in crate::state) const RESOURCE_MEMBERS: &[&str] = &[
    "Catalog",
    "Database",
    "Table",
    "TableWithColumns",
    "DataLocation",
    "DataCellsFilter",
    "LFTag",
    "LFTagPolicy",
];

/// The legacy type's spelling of each `Resource` member it publishes, paired with the member
/// of that service's `Resource` shape it is.
///
/// The legacy type's reference names the four Data Catalog kinds a grant can be made on with
/// a `…Resource` suffix; the newer type names them as the API does.
const LEGACY_RESOURCE_MEMBERS: &[(&str, &str)] = &[
    ("CatalogResource", "Catalog"),
    ("DatabaseResource", "Database"),
    ("TableResource", "Table"),
    ("TableWithColumnsResource", "TableWithColumns"),
    ("DataLocationResource", "DataLocation"),
];

/// The legacy type's spelling of a data location's ARN, and the member
/// `DataLocationResource` takes it as ("the Amazon Resource Name (ARN) that uniquely
/// identifies the data location resource").
const LEGACY_S3_RESOURCE_MEMBER: &str = "S3Resource";
const DATA_LOCATION_ARN_MEMBER: &str = "ResourceArn";

/// AWS Glue's ARN namespace (that model's `awsService.arnNamespace`), which the ARN of a Data
/// Catalog resource carries: the Data Catalog is Glue's, and a grant's legacy identifier is
/// built around the ARN of the entity it is on.
const GLUE_ARN_NAMESPACE: &str = "glue";

/// The resource paths a Data Catalog entity's ARN carries: the catalog itself, a database in
/// it, and a table of a database.
const CATALOG_ARN_RESOURCE: &str = "catalog";
const DATABASE_ARN_SEGMENT: &str = "database";
const TABLE_ARN_SEGMENT: &str = "table";

/// The separator the legacy type's identifier joins the principal and the resource ARN with,
/// and the prefix it carries.
const LEGACY_IDENTITY_PREFIX: &str = "LakeFormation-";
const LEGACY_IDENTITY_SEPARATOR: &str = "::";

/// The members of a `Resource` member that name the Data Catalog entity the ARN is built
/// from.
const NAME_MEMBER: &str = "Name";
const DATABASE_NAME_MEMBER: &str = "DatabaseName";

/// The most pages a drift read walks the account's grants in
/// ([`describe_lake_formation_permissions`]).
///
/// The listing is paged ("`MaxResults` — the maximum number of results to return",
/// `range 1..1000`), and the read asks for the published maximum per page, so this bounds a
/// walk over an account holding far more grants than one resource's check needs to see rather
/// than following a token for as long as the service offers one.
const MAX_LISTED_PAGES: usize = 10;

/// The page size a drift read asks for: the published maximum of `ListPermissions$MaxResults`
/// (`range 1..1000`), so the walk makes as few calls as that operation allows.
const LISTED_PAGE_SIZE: i64 = 1000;

/// The members a `ListPermissions` answer carries.
const PRINCIPAL_RESOURCE_PERMISSIONS_MEMBER: &str = "PrincipalResourcePermissions";
const NEXT_TOKEN_MEMBER: &str = "NextToken";
const MAX_RESULTS_MEMBER: &str = "MaxResults";

/// The error code this module reports a declaration that describes no grant at all under.
const MISSING_PROPERTY_CODE: &str = "InvalidRequest";

/// One grant as both types describe it, read off the evaluated property document: the request
/// Lake Formation's own calls take.
#[derive(Debug, Clone)]
struct DeclaredGrant {
    /// The Data Catalog the grant is made in, or `None` for that member's own documented
    /// default (the account's own catalog).
    catalog_id: Option<String>,
    /// The principal the grant is made to, as `DataLakePrincipal`.
    principal: Value,
    /// The Data Catalog resource it is on, as `Resource`.
    resource: Value,
    /// What the principal holds.
    permissions: Vec<Value>,
    /// What it holds WITH the grant option.
    permissions_with_grant_option: Vec<Value>,
}

impl DeclaredGrant {
    /// The request `GrantPermissions` and `RevokePermissions` both take, carrying
    /// `permissions` and `with_grant_option` rather than this grant's own lists: an update
    /// grants and revokes the DIFFERENCE between two declarations, so the one request builder
    /// serves both halves of it.
    fn request(&self, permissions: &[Value], with_grant_option: &[Value]) -> Value {
        let mut request = Map::new();
        if let Some(catalog_id) = &self.catalog_id {
            request.insert(
                CATALOG_ID_MEMBER.to_string(),
                Value::String(catalog_id.clone()),
            );
        }
        request.insert(PRINCIPAL_MEMBER.to_string(), self.principal.clone());
        request.insert(RESOURCE_MEMBER.to_string(), self.resource.clone());
        request.insert(
            PERMISSIONS_MEMBER.to_string(),
            Value::Array(permissions.to_vec()),
        );
        request.insert(
            PERMISSIONS_WITH_GRANT_OPTION_MEMBER.to_string(),
            Value::Array(with_grant_option.to_vec()),
        );
        Value::Object(request)
    }

    /// The whole grant as the request that makes it.
    fn granted(&self) -> Value {
        self.request(&self.permissions, &self.permissions_with_grant_option)
    }

    /// The principal's own identifier, as the records of a listing report it.
    fn principal_identifier(&self) -> Option<String> {
        self.principal
            .get(DATA_LAKE_PRINCIPAL_IDENTIFIER_MEMBER)
            .and_then(scalar_text)
    }
}

/// The grant one declaration of `grant`'s type describes, or the refusal a declaration that
/// describes none gets: a grant is made TO a principal ON a resource, so a declaration
/// missing either names nothing that could be granted or ever revoked.
///
/// The permission lists are passed through exactly as declared, EMPTY where the declaration
/// states none: which values name a permission is Lake Formation's own to judge
/// (`Permission`, that model's enum), and a grant of no permission is that service's own
/// refusal rather than one invented here.
fn declared_grant(grant: Grant, properties: &Value) -> Result<DeclaredGrant, Refusal> {
    let missing = |member: &str| Refusal {
        code: MISSING_PROPERTY_CODE.to_string(),
        message: format!("{} declares no {member}", type_name(grant)),
    };
    let principal_property = match grant {
        Grant::Legacy => DATA_LAKE_PRINCIPAL_PROPERTY,
        Grant::Principal => PRINCIPAL_MEMBER,
    };
    let principal = properties
        .get(principal_property)
        .filter(|declared| declared.is_object())
        .cloned()
        .ok_or_else(|| missing(principal_property))?;
    let declared_resource = properties
        .get(RESOURCE_MEMBER)
        .filter(|declared| declared.is_object())
        .ok_or_else(|| missing(RESOURCE_MEMBER))?;
    Ok(DeclaredGrant {
        catalog_id: match grant {
            // The legacy type publishes no catalog property at all, so a grant it declares is
            // made in the catalog that member's own default names.
            Grant::Legacy => None,
            Grant::Principal => properties.get(CATALOG_PROPERTY).and_then(scalar_text),
        },
        principal,
        resource: resource_document(grant, declared_resource),
        permissions: declared_values(properties, PERMISSIONS_MEMBER),
        permissions_with_grant_option: declared_values(
            properties,
            PERMISSIONS_WITH_GRANT_OPTION_MEMBER,
        ),
    })
}

/// The type a grant of this kind is declared with, for the refusal above.
fn type_name(grant: Grant) -> &'static str {
    match grant {
        Grant::Legacy => LAKEFORMATION_PERMISSIONS,
        Grant::Principal => LAKEFORMATION_PRINCIPAL_PERMISSIONS,
    }
}

/// The declared `Resource` as the `Resource` Lake Formation's calls take: the newer type's
/// document unchanged, and the legacy type's with each member renamed to the member of that
/// shape it is ([`LEGACY_RESOURCE_MEMBERS`], and a data location's ARN to
/// [`DATA_LOCATION_ARN_MEMBER`]).
///
/// A member neither spelling names is carried through as declared: it is no member of the
/// type, and the service that owns the call is what judges it.
fn resource_document(grant: Grant, declared: &Value) -> Value {
    if grant == Grant::Principal {
        return declared.clone();
    }
    let Value::Object(members) = declared else {
        return declared.clone();
    };
    let mut resource = Map::new();
    for (member, value) in members {
        match LEGACY_RESOURCE_MEMBERS
            .iter()
            .find(|(legacy, _)| legacy == member)
        {
            Some((legacy, named)) => {
                let value = if *legacy == "DataLocationResource" {
                    data_location_document(value)
                } else {
                    value.clone()
                };
                resource.insert((*named).to_string(), value);
            }
            None => {
                resource.insert(member.clone(), value.clone());
            }
        }
    }
    Value::Object(resource)
}

/// One legacy `DataLocationResource` as the `DataLocationResource` that service's own call
/// takes: its `S3Resource` under the `ResourceArn` that shape names it, and every other
/// member as declared.
fn data_location_document(declared: &Value) -> Value {
    let Value::Object(members) = declared else {
        return declared.clone();
    };
    let mut location = Map::new();
    for (member, value) in members {
        let named = if member == LEGACY_S3_RESOURCE_MEMBER {
            DATA_LOCATION_ARN_MEMBER
        } else {
            member.as_str()
        };
        location.insert(named.to_string(), value.clone());
    }
    Value::Object(location)
}

/// The elements one declared LIST property carries, and none where the declaration states no
/// list at all.
fn declared_values(properties: &Value, property: &str) -> Vec<Value> {
    match properties.get(property) {
        Some(Value::Array(values)) => values.clone(),
        _ => Vec::new(),
    }
}

/// Grant the permissions a declaration of either type describes, so a client that deployed
/// the stack and then calls `lakeformation ListPermissions` for that principal reads the
/// grant back rather than nothing.
///
/// The declared document IS the request ([`declared_grant`]), and the identity is the pair
/// the naming rules already computed for the resource, so nothing here replaces it: the
/// create answers no identifier of its own ("`GrantPermissionsResponse`" models no member).
pub(super) async fn grant_lake_formation_permissions(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    grant: Grant,
    resource: &StoredResource,
) -> Result<Created, Refusal> {
    let declared = declared_grant(grant, &resource.properties)?;
    call_lake_formation(
        ctx,
        region,
        GRANT_PERMISSIONS,
        GRANT_PERMISSIONS_URI,
        &declared.granted(),
    )
    .await?;
    Ok(Created::default())
}

/// Revoke exactly the permissions the stack's own grant carries, by the pair it was made
/// under — so a permission the stack did not grant (a database's own default grant, the
/// catalog's) is left exactly as it is.
///
/// A revoke of a grant Lake Formation no longer holds is already in the state the delete
/// asks for: that service answers such a revoke with its `EntityNotFoundException`, judged on
/// the CODE and never on a message that carries the request's own values.
pub(super) async fn revoke_lake_formation_permissions(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    grant: Grant,
    resource: &StoredResource,
) -> Result<(), Refusal> {
    let declared = declared_grant(grant, &resource.properties)?;
    match call_lake_formation(
        ctx,
        region,
        REVOKE_PERMISSIONS,
        REVOKE_PERMISSIONS_URI,
        &declared.granted(),
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == LAKEFORMATION_ENTITY_NOT_FOUND => Ok(()),
        Err(refusal) => Err(refusal),
    }
}

/// The error code Lake Formation answers for an entity it cannot resolve
/// (`EntityNotFoundException`, the modelled 400 both calls declare).
const LAKEFORMATION_ENTITY_NOT_FOUND: &str = "EntityNotFoundException";

/// Apply a CHANGED declaration to the grant the stack holds: the revoke of every permission
/// it no longer carries, and the grant of every one it adds.
///
/// Those are the two calls Lake Formation publishes for a grant that exists — it has no
/// "set the permissions to this" operation — so a declaration whose permission lists changed
/// is deployed as the difference between them, and a permission both declarations carry is
/// not re-sent. The principal, the resource and the catalog are the pair that IDENTIFIES the
/// grant, so a declaration changing one of those replaces the resource and never reaches
/// here.
pub(super) async fn update_lake_formation_permissions(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    grant: Grant,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Option<Created>, Refusal> {
    let held = declared_grant(grant, &resource.properties)?;
    let wanted = declared_grant(grant, declared)?;
    let revoked = withdrawn(&held.permissions, &wanted.permissions);
    let revoked_with_grant_option = withdrawn(
        &held.permissions_with_grant_option,
        &wanted.permissions_with_grant_option,
    );
    if !revoked.is_empty() || !revoked_with_grant_option.is_empty() {
        call_lake_formation(
            ctx,
            region,
            REVOKE_PERMISSIONS,
            REVOKE_PERMISSIONS_URI,
            &wanted.request(&revoked, &revoked_with_grant_option),
        )
        .await?;
    }
    let granted = withdrawn(&wanted.permissions, &held.permissions);
    let granted_with_grant_option = withdrawn(
        &wanted.permissions_with_grant_option,
        &held.permissions_with_grant_option,
    );
    if !granted.is_empty() || !granted_with_grant_option.is_empty() {
        call_lake_formation(
            ctx,
            region,
            GRANT_PERMISSIONS,
            GRANT_PERMISSIONS_URI,
            &wanted.request(&granted, &granted_with_grant_option),
        )
        .await?;
    }
    Ok(Some(Created::default()))
}

/// The values `held` carries that `wanted` does not: what a changed declaration takes away.
fn withdrawn(held: &[Value], wanted: &[Value]) -> Vec<Value> {
    held.iter()
        .filter(|value| !wanted.contains(value))
        .cloned()
        .collect()
}

/// The grant as Lake Formation reports it, for drift detection: the two permission lists the
/// record for this principal on this resource carries, and `None` where that service holds no
/// such record at all — which is the `DELETED` drift status.
///
/// Read from the listing of the RESOURCE rather than of the (principal, resource) pair,
/// because the latter "returns effective permissions rather than the explicitly granted
/// permissions" (`ListPermissions`) — which includes what a GROUP the principal belongs to
/// holds, and what a group holds is no part of this resource's own configuration.
pub(super) async fn describe_lake_formation_permissions(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    grant: Grant,
    resource: &StoredResource,
) -> Result<Option<Value>, Refusal> {
    let declared = declared_grant(grant, &resource.properties)?;
    let Some(principal) = declared.principal_identifier() else {
        return Ok(None);
    };
    let mut token: Option<String> = None;
    for _page in 0..MAX_LISTED_PAGES {
        let mut request = Map::new();
        if let Some(catalog_id) = &declared.catalog_id {
            request.insert(
                CATALOG_ID_MEMBER.to_string(),
                Value::String(catalog_id.clone()),
            );
        }
        request.insert(RESOURCE_MEMBER.to_string(), declared.resource.clone());
        request.insert(
            MAX_RESULTS_MEMBER.to_string(),
            Value::from(LISTED_PAGE_SIZE),
        );
        if let Some(token) = &token {
            request.insert(NEXT_TOKEN_MEMBER.to_string(), Value::String(token.clone()));
        }
        let answered = call_lake_formation(
            ctx,
            region,
            LIST_PERMISSIONS,
            LIST_PERMISSIONS_URI,
            &Value::Object(request),
        )
        .await?;
        if let Some(reported) = reported_grant(&answered, &principal) {
            return Ok(Some(reported));
        }
        match answered.get(NEXT_TOKEN_MEMBER).and_then(scalar_text) {
            Some(next) if !next.is_empty() => token = Some(next),
            _ => return Ok(None),
        }
    }
    Ok(None)
}

/// The record one listing page reports for `principal`, as the property document the two
/// types state a grant's permissions in; `None` where the page carries no record for it.
fn reported_grant(answered: &Value, principal: &str) -> Option<Value> {
    let Some(Value::Array(records)) = answered.get(PRINCIPAL_RESOURCE_PERMISSIONS_MEMBER) else {
        return None;
    };
    let record = records.iter().find(|record| {
        record
            .get(PRINCIPAL_MEMBER)
            .and_then(|reported| reported.get(DATA_LAKE_PRINCIPAL_IDENTIFIER_MEMBER))
            .and_then(scalar_text)
            .as_deref()
            == Some(principal)
    })?;
    let mut reported = Map::new();
    for member in [PERMISSIONS_MEMBER, PERMISSIONS_WITH_GRANT_OPTION_MEMBER] {
        reported.insert(
            member.to_string(),
            record.get(member).cloned().unwrap_or(Value::Array(Vec::new())),
        );
    }
    Some(Value::Object(reported))
}

/// Call one Lake Formation operation on the `restJson1` wire its model declares, naming the
/// operation so a refusal is reported the way that service's own SDK raises it.
async fn call_lake_formation(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    operation: &str,
    uri: &str,
    request: &Value,
) -> Result<Value, Refusal> {
    call_rest_json_named(
        ctx,
        LAKEFORMATION_SIGNING_NAME,
        region,
        operation,
        "POST",
        uri,
        Some(request),
    )
    .await
}

/// The newer type's `PrincipalIdentifier`: the principal half of its composite primary
/// identifier, which is the `DataLakePrincipal` the grant was made to as its own structure.
///
/// Rendered as the document that shape is — one member,
/// [`DATA_LAKE_PRINCIPAL_IDENTIFIER_MEMBER`] — so a template reading it with `Fn::GetAtt`
/// gets the principal the grant names, and `null` where the declaration states none.
pub(in crate::state) fn principal_identifier(properties: &Value) -> String {
    let identifier = properties
        .get(PRINCIPAL_MEMBER)
        .and_then(|principal| principal.get(DATA_LAKE_PRINCIPAL_IDENTIFIER_MEMBER))
        .cloned()
        .unwrap_or(Value::Null);
    document(&[(DATA_LAKE_PRINCIPAL_IDENTIFIER_MEMBER, identifier)])
}

/// The newer type's `ResourceIdentifier`: the resource half of that identifier, which is the
/// `Resource` the grant is on as its own structure.
///
/// Every member of the type's `Resource` is present ([`RESOURCE_MEMBERS`], in the order that
/// property's own reference lists them), the one the declaration states carrying its value
/// and the rest `null`: the identifier describes which KIND of Data Catalog resource the
/// grant is on, and that is said by which member is populated.
pub(in crate::state) fn resource_identifier(properties: &Value) -> String {
    let declared = properties.get(RESOURCE_MEMBER);
    let members: Vec<(&str, Value)> = RESOURCE_MEMBERS
        .iter()
        .map(|member| {
            (
                *member,
                declared
                    .and_then(|resource| resource.get(member))
                    .cloned()
                    .unwrap_or(Value::Null),
            )
        })
        .collect();
    document(&members)
}

/// One JSON object as the TEXT an `Fn::GetAtt` of a read-only property answers, with its
/// members in the order given.
///
/// The members are written in the order the type's own property reference publishes them
/// rather than in whatever order a map holds them in, because this document is part of the
/// resource's IDENTITY: the identifier a `Ref` resolves to has to be the same string on every
/// read and in every build. Each member's name and value is still rendered by the JSON
/// serialiser ([`Value::to_string`]), so nothing here decides how a value is escaped.
fn document(members: &[(&str, Value)]) -> String {
    let mut rendered = String::from("{");
    for (index, (member, value)) in members.iter().enumerate() {
        if index > 0 {
            rendered.push(',');
        }
        rendered.push_str(&Value::String((*member).to_string()).to_string());
        rendered.push(':');
        rendered.push_str(&value.to_string());
    }
    rendered.push('}');
    rendered
}

/// The legacy type's physical resource id: the principal the grant was made to and the ARN of
/// the Data Catalog resource it is on, which together are the only thing that identifies one
/// grant.
///
/// `None` for a declaration that states neither, which describes no grant at all: such a
/// resource keeps the identifier the naming rules computed, and the owning service's own
/// validation of the members it is missing is what judges it.
pub(in crate::state) fn legacy_identity(properties: &Value, region: &str) -> Option<String> {
    let principal = properties
        .get(DATA_LAKE_PRINCIPAL_PROPERTY)
        .and_then(|principal| principal.get(DATA_LAKE_PRINCIPAL_IDENTIFIER_MEMBER))
        .and_then(scalar_text)?;
    let resource = legacy_resource_arn(properties.get(RESOURCE_MEMBER)?, region)?;
    Some(format!(
        "{LEGACY_IDENTITY_PREFIX}{principal}{LEGACY_IDENTITY_SEPARATOR}{resource}"
    ))
}

/// The ARN of the Data Catalog resource one legacy `Resource` names: the catalog itself, a
/// database in it, a table of a database (named by a `TableResource` or a
/// `TableWithColumnsResource`), or the data location an S3 ARN already names.
///
/// The Data Catalog is AWS Glue's, so the ARN is one of that service's: the catalog id the
/// member states is the ACCOUNT the entity belongs to, which is that member's own meaning
/// ("the identifier for the Data Catalog. By default, it is the account ID of the caller").
fn legacy_resource_arn(declared: &Value, region: &str) -> Option<String> {
    for (legacy, _) in LEGACY_RESOURCE_MEMBERS {
        let Some(named) = declared.get(legacy) else {
            continue;
        };
        let catalog = named.get(CATALOG_ID_MEMBER).and_then(scalar_text);
        let path = match *legacy {
            "CatalogResource" => Some(CATALOG_ARN_RESOURCE.to_string()),
            "DatabaseResource" => named
                .get(NAME_MEMBER)
                .and_then(scalar_text)
                .map(|name| format!("{DATABASE_ARN_SEGMENT}/{name}")),
            "TableResource" | "TableWithColumnsResource" => {
                let database = named.get(DATABASE_NAME_MEMBER).and_then(scalar_text)?;
                named
                    .get(NAME_MEMBER)
                    .and_then(scalar_text)
                    .map(|name| format!("{TABLE_ARN_SEGMENT}/{database}/{name}"))
            }
            // A data location is already named by the ARN of the Amazon S3 location itself.
            _ => return named.get(LEGACY_S3_RESOURCE_MEMBER).and_then(scalar_text),
        }?;
        return Some(glue_arn(region, catalog.as_deref(), &path));
    }
    None
}

/// A Data Catalog entity's ARN: the canonical layout the shared minter builds, with the
/// ACCOUNT field replaced by the catalog id where a declaration states one — a grant may be
/// made on an entity of another account's catalog, and that account is what such an ARN
/// names.
fn glue_arn(region: &str, catalog_id: Option<&str>, resource: &str) -> String {
    let canonical = ::emulator_server::emulator_arn(GLUE_ARN_NAMESPACE, region, resource);
    let Some(catalog_id) = catalog_id.filter(|catalog| !catalog.is_empty()) else {
        return canonical;
    };
    let mut fields: Vec<&str> = canonical.split(':').collect();
    // Field 5 of an ARN is the account; the canonical form always carries it.
    if let Some(account) = fields.get_mut(4) {
        *account = catalog_id;
    }
    fields.join(":")
}
