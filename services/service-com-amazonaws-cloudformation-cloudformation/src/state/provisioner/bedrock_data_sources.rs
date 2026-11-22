// SPDX-License-Identifier: CC0-1.0

//! The per-type provisioning arms of the DATA SOURCE an `AWS::Bedrock::DataSource` attaches to
//! a knowledge base of Amazon Bedrock Agent's build-time control plane. One file of [`super`]'s
//! per-type arms, dispatched to by its `Provisioner`, beside the knowledge base's own arms
//! ([`super::bedrock_knowledge_bases`]).
//!
//! ## Why the data source is really created
//!
//! A data source is what a knowledge base ingests FROM, and `dataSourceId` is the handle every
//! call on one takes — `GetDataSource`, `UpdateDataSource`, `DeleteDataSource`,
//! `StartIngestionJob` and `ListIngestionJobs` all address one by it, beside the knowledge
//! base's own id — while `ListDataSources` is how a client DISCOVERS the data sources of a
//! knowledge base at all. A stack that reported the data source created while nothing created
//! it publishes a handle that resolves to nothing and leaves that listing empty, so the first
//! call a client makes with the deployed stack fails and anything reconciling the stack's
//! contents against that service concludes the data source was deleted out of band. Creating
//! the data source where it belongs is what makes the stack's report of it true.
//!
//! ## The identity is the PAIR, and the id inside it is the service's
//!
//! `CreateDataSource` takes the data source's NAME and answers the `dataSourceId` it minted —
//! modelled `^[0-9a-zA-Z]{10}$`, exactly ten alphanumeric characters — which no name
//! CloudFormation decides can satisfy. The type's primary identifier is that id TOGETHER with
//! the knowledge base the data source belongs to, which are exactly the two labels every call
//! on a data source binds, so this resource's physical id is the two joined
//! ([`template::bedrock_data_source_identity`]) and
//! [`super::super::template::identity_is_service_assigned`] names the type: the pair the
//! naming rules computed is only what a reference was resolved against until the create
//! answered.
//!
//! ## What the declaration is
//!
//! Every property of this type is the member of the same name with a lower-cased initial all
//! the way down (`DataSourceConfiguration.S3Configuration.BucketArn` is
//! `dataSourceConfiguration.s3Configuration.bucketArn`), so the declared document IS the
//! request once its member names are re-cased ([`camel_cased`]) — the same relation the
//! knowledge base's arms beside this one rely on — with the one property the URI carries
//! lifted out of it. The type publishes no `Tags` property and `CreateDataSource` takes no
//! `tags` member, so CloudFormation propagates no tag to a data source.
//!
//! No `clientToken` is sent. That member is an `@idempotencyToken`, so a token derived from
//! the resource would make the create of a REPLACEMENT data source replay the one the stack
//! already had instead of attaching a new one.
//!
//! ## A refusal about the knowledge base the declaration REFERENCES
//!
//! The create names TWO resources: the data source it makes, and the knowledge base it asks to
//! attach that data source to — which the declaration only references, by the id stated in
//! `KnowledgeBaseId`. A `ResourceNotFoundException` answered to the create can therefore be
//! about the referenced knowledge base alone, the data source not existing yet, and this
//! provider reports such a refusal in the resource-provider plugin's own not-found sentence
//! naming that resource's CloudFormation type and the stated identifier
//! ([`referenced_knowledge_base_not_found`]) rather than forwarding the owning service's
//! sentence, which names no CloudFormation type at all.

use super::*;

/// The member `CreateDataSource` and `UpdateDataSource` take the data source's name in, which
/// is the one member CloudFormation decides.
const NAME_MEMBER: &str = "name";

/// The member the answer carries the data source in: the create, the read and the update each
/// answer a `dataSource` structure.
const DATA_SOURCE_MEMBER: &str = "dataSource";

/// The property the declaration states the data source's name in.
const NAME_PROPERTY: &str = "Name";

/// The `DataSourceStatus` a data source is ready in: "AVAILABLE – The data source has been
/// created and is ready for ingestion into the knowledge base".
const DATA_SOURCE_AVAILABLE: &str = "AVAILABLE";

/// The transient `DataSourceStatus` values a data source is still settling in: "CREATING – The
/// data source is being created" and "UPDATING – The data source is being updated".
const DATA_SOURCE_COMING_UP: [&str; 2] = ["CREATING", "UPDATING"];

/// The `DataSourceStatus` a data source being removed is reported in: "DELETING – The data
/// source is being deleted".
const DATA_SOURCE_DELETING: &str = "DELETING";

/// The members an answered data source reports, as `(attribute, member)`: the values this type
/// publishes as return values, read off the answer rather than recomputed here.
///
/// `failureReasons` is not among them — it is a LIST, and the attribute map is string-valued,
/// so reporting one entry as the whole of it would be worse than reporting none; the reasons
/// are read for the diagnosis of a data source that FAILED instead ([`data_source_readiness`]).
const DATA_SOURCE_ATTRIBUTES: &[(&str, &str)] = &[
    (template::BEDROCK_DATA_SOURCE_ID_ATTRIBUTE, "dataSourceId"),
    ("DataSourceStatus", "status"),
    ("CreatedAt", "createdAt"),
    ("UpdatedAt", "updatedAt"),
];

/// The properties a data source's own read reports back under the same names, for drift
/// detection, as `(property, member)`: the scalars `GetDataSource` answers that a declaration
/// also states.
///
/// The connection configuration document is left out: what the read reports are that service's
/// own member names, and a property the read does not report is not evidence of anything
/// ([`super::drift`]).
const DATA_SOURCE_REPORTED_PROPERTIES: &[(&str, &str)] = &[
    ("KnowledgeBaseId", "knowledgeBaseId"),
    ("Name", "name"),
    ("Description", "description"),
    ("DataDeletionPolicy", "dataDeletionPolicy"),
];

/// Create the data source an `AWS::Bedrock::DataSource` declares, so a client that deployed the
/// stack finds it in `ListDataSources` for the knowledge base the stack published and reads it
/// back through `GetDataSource` by the id the stack handed out — and so an ingestion job can be
/// started for the data source the stack reported created.
///
/// The identity is Amazon Bedrock Agent's: the `dataSourceId` it minted, joined with the
/// knowledge base the data source was attached to, becomes this resource's physical id — the
/// primary identifier of the type — and the id, the status and the two instants it answered are
/// the resource's attributes.
pub(super) async fn create_bedrock_data_source(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<Created, Refusal> {
    // The knowledge base is read off the DECLARATION here, not off the identity: the
    // declaration is the resolved request — a reference to a knowledge base of the same stack
    // carries the id that service minted by now — while the identity the naming rules computed
    // is only a stand-in until this create answers.
    let knowledge_base_id = declared_scalar(
        &resource.properties,
        template::BEDROCK_DATA_SOURCE_KNOWLEDGE_BASE_ID_PROPERTY,
    );
    let mut request = declared_data_source_members(&resource.properties)?;
    request.insert(
        NAME_MEMBER.to_string(),
        Value::String(data_source_name(&resource.properties, resource)),
    );
    let answered = call_rest_json(
        ctx,
        BEDROCK_SIGNING_NAME,
        region,
        "PUT",
        &data_sources_uri(&knowledge_base_id),
        Some(&Value::Object(request)),
    )
    .await
    .map_err(|refusal| referenced_knowledge_base_not_found(refusal, &knowledge_base_id))?;
    let attributes = data_source_attributes(&answered)?;
    Ok(Created {
        physical_resource_id: Some(template::bedrock_data_source_identity(
            &knowledge_base_id,
            attributes
                .get(template::BEDROCK_DATA_SOURCE_ID_ATTRIBUTE)
                .map(String::as_str)
                .unwrap_or_default(),
        )),
        attributes,
    })
}

/// How this type's provider reports a create Amazon Bedrock Agent refused because the
/// KNOWLEDGE BASE the declaration REFERENCES is not there: in the resource-provider plugin's
/// own not-found sentence ([`provider_resource_not_found`]), naming the CloudFormation TYPE of
/// the resource that is missing — the knowledge base's, not the data source's — and the
/// identifier the declaration stated for it.
///
/// The create is the one call on a data source whose not-found answer can be about nothing
/// else: the data source does not exist yet, so the only resource the request names is the
/// knowledge base it asks to attach to, which is the resource the declaration references. The
/// `knowledgeBaseId` the declaration stated is therefore the identifier the refusal is about,
/// carried through exactly as the template wrote it.
///
/// Judged on the CODE Amazon Bedrock Agent answered ([`BEDROCK_RESOURCE_NOT_FOUND`]), never on
/// its message — the message carries the request's own values, while the code is what says
/// what kind of refusal this is. Every other refusal is reported as that service worded it,
/// and the CODE travels untouched either way, so the `HandlerErrorCode` class the event
/// carries is the one the refusal falls in.
fn referenced_knowledge_base_not_found(refusal: Refusal, knowledge_base_id: &str) -> Refusal {
    if refusal.code != BEDROCK_RESOURCE_NOT_FOUND {
        return refusal;
    }
    Refusal {
        message: provider_resource_not_found(template::BEDROCK_KNOWLEDGE_BASE, knowledge_base_id),
        code: refusal.code,
    }
}

/// Whether Amazon Bedrock Agent has finished settling the data source a stack created.
///
/// The modelled `DataSource$status` documents exactly one value as the status of a data source
/// that "has been created and is ready for ingestion into the knowledge base" — `AVAILABLE` —
/// so that is the state the resource's creation is finished in, `CREATING` and `UPDATING` the
/// states it is still settling in, and every other status is one no wait will bring it out of:
/// a data source that failed, or one being deleted out of this stack's reach. One that service
/// no longer holds is in the same position, nothing being left to create it. The reasons that
/// service reports are the diagnosis of a failure, so they travel with it.
pub(super) async fn data_source_readiness(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Readiness, Refusal> {
    let Some(data_source) = read_data_source(ctx, region, physical_resource_id).await? else {
        return Ok(Readiness::Failed(format!(
            "Amazon Bedrock holds no data source {physical_resource_id}"
        )));
    };
    let status = reported_text(&data_source, "status").unwrap_or_default();
    if status == DATA_SOURCE_AVAILABLE {
        return Ok(Readiness::Ready);
    }
    if DATA_SOURCE_COMING_UP.contains(&status.as_str()) {
        return Ok(Readiness::Pending(status));
    }
    Ok(Readiness::Failed(failed_reason(
        physical_resource_id,
        &status,
        &data_source,
    )))
}

/// The attributes the owning service publishes for a data source it now reports up: its own
/// read of the data source, whose `status` and `updatedAt` are not the ones the create answered
/// with ([`DATA_SOURCE_ATTRIBUTES`]).
pub(super) async fn data_source_ready_attributes(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<BTreeMap<String, String>, Refusal> {
    let Some(data_source) = read_data_source(ctx, region, physical_resource_id).await? else {
        return Ok(BTreeMap::new());
    };
    Ok(reported_attributes(&data_source, DATA_SOURCE_ATTRIBUTES))
}

/// Whether Amazon Bedrock Agent has finished removing the data source a stack deleted.
///
/// `DeleteDataSource` answers `202 Accepted` with the data source `DELETING` — "the data source
/// is being deleted" — so it is still there when the delete answers, and the deletion is
/// finished once that service no longer holds it.
pub(super) async fn data_source_removal(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Removal, Refusal> {
    match read_data_source(ctx, region, physical_resource_id).await? {
        None => Ok(Removal::Gone),
        Some(data_source) => Ok(Removal::Pending(
            reported_text(&data_source, "status")
                .unwrap_or_else(|| DATA_SOURCE_DELETING.to_string()),
        )),
    }
}

/// Apply a changed declaration to the data source the stack created, through
/// `UpdateDataSource` — the one call that changes a data source that exists.
///
/// It takes the knowledge base and the data source's own id together with the whole declared
/// configuration, so a member the declaration no longer states leaves the data source without
/// it rather than keeping the one an earlier deployment gave it. The data source keeps the id
/// that service minted, so the handle the stack publishes is unchanged.
///
/// `Ok(None)` is a change that call cannot make, so the deployment reports it as not applied
/// rather than as a success that dropped it: a declaration that re-points the data source at
/// ANOTHER knowledge base, which no call moves a data source between — that is a different
/// data source, under a different identity.
pub(super) async fn update_bedrock_data_source(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Option<Created>, Refusal> {
    let (knowledge_base_id, data_source_id) = data_source_identity(resource);
    if declared_scalar(
        declared,
        template::BEDROCK_DATA_SOURCE_KNOWLEDGE_BASE_ID_PROPERTY,
    ) != knowledge_base_id
    {
        return Ok(None);
    }
    let mut request = declared_data_source_members(declared)?;
    request.insert(
        NAME_MEMBER.to_string(),
        Value::String(data_source_name(declared, resource)),
    );
    let answered = call_rest_json(
        ctx,
        BEDROCK_SIGNING_NAME,
        region,
        "PUT",
        &data_source_uri(&knowledge_base_id, &data_source_id),
        Some(&Value::Object(request)),
    )
    .await?;
    Ok(Some(Created::attributes(data_source_attributes(&answered)?)))
}

/// Delete the data source a stack created, by the knowledge base and the id it was created
/// under — the two parts of this resource's physical id. One the service no longer holds
/// (`ResourceNotFoundException`, judged on the CODE and never on a message that carries the
/// request's own values) is already in the state the delete asks for.
pub(super) async fn delete_bedrock_data_source(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<(), Refusal> {
    let (knowledge_base_id, data_source_id) = data_source_identity(resource);
    match call_rest_json(
        ctx,
        BEDROCK_SIGNING_NAME,
        region,
        "DELETE",
        &data_source_uri(&knowledge_base_id, &data_source_id),
        None,
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == BEDROCK_RESOURCE_NOT_FOUND => Ok(()),
        Err(refusal) => Err(refusal),
    }
}

/// The data source's configuration as Amazon Bedrock Agent reports it, for drift detection:
/// `GetDataSource`'s own answer, read by the pair that is this resource's physical id
/// ([`DATA_SOURCE_REPORTED_PROPERTIES`]). One it no longer holds was deleted.
pub(super) async fn describe_bedrock_data_source(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<Option<Value>, Refusal> {
    Ok(read_data_source(ctx, region, &resource.physical_resource_id)
        .await?
        .map(|data_source| {
            reported_as_properties(&data_source, DATA_SOURCE_REPORTED_PROPERTIES)
        }))
}

/// The data source Amazon Bedrock Agent holds under `physical_resource_id`, or `None` when it
/// holds none (`ResourceNotFoundException`, judged on the CODE and never on a message).
///
/// A knowledge base that service no longer holds answers the same code for a data source of
/// it, which is the same answer: there is no data source there either.
async fn read_data_source(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    let Some((knowledge_base_id, data_source_id)) =
        template::bedrock_data_source_identity_parts(physical_resource_id)
    else {
        return Ok(None);
    };
    match call_rest_json(
        ctx,
        BEDROCK_SIGNING_NAME,
        region,
        "GET",
        &data_source_uri(knowledge_base_id, data_source_id),
        None,
    )
    .await
    {
        Ok(answer) => Ok(answer.get(DATA_SOURCE_MEMBER).cloned()),
        Err(refusal) if refusal.code == BEDROCK_RESOURCE_NOT_FOUND => Ok(None),
        Err(refusal) => Err(refusal),
    }
}

/// The URI the data source COLLECTION of one knowledge base is bound to
/// (`PUT`/`POST /knowledgebases/{knowledgeBaseId}/datasources/`, trailing slash included, as
/// `CreateDataSource`'s and `ListDataSources`' own `@http` bindings spell it).
///
/// The knowledge base is escaped as a single path segment ([`percent_encoded`]) so a value
/// carrying URI structure addresses one knowledge base rather than another path.
fn data_sources_uri(knowledge_base_id: &str) -> String {
    format!(
        "/knowledgebases/{}/datasources/",
        percent_encoded(knowledge_base_id)
    )
}

/// The URI ONE data source is addressed at: its collection plus the `dataSourceId`
/// `@httpLabel`, escaped the same way.
fn data_source_uri(knowledge_base_id: &str, data_source_id: &str) -> String {
    format!(
        "{}{}",
        data_sources_uri(knowledge_base_id),
        percent_encoded(data_source_id)
    )
}

/// The knowledge base and the data source id one resource record names: the two parts of the
/// identity the type is addressed by.
///
/// Read off the physical resource id where that is such an identity
/// ([`template::bedrock_data_source_identity_parts`]) — which it is once the create has
/// answered, and for the stand-in the naming rules computed. A record that carries anything
/// else falls back to what the declaration states for the knowledge base and to the
/// `DataSourceId` the resource publishes.
fn data_source_identity(resource: &StoredResource) -> (String, String) {
    let identity = template::bedrock_data_source_identity_parts(&resource.physical_resource_id)
        // An identity whose knowledge base or id is EMPTY is the stand-in computed for a
        // declaration that was not resolved yet, which addresses no data source.
        .filter(|(knowledge_base_id, data_source_id)| {
            !knowledge_base_id.is_empty() && !data_source_id.is_empty()
        });
    if let Some((knowledge_base_id, data_source_id)) = identity {
        return (knowledge_base_id.to_string(), data_source_id.to_string());
    }
    (
        declared_scalar(
            &resource.properties,
            template::BEDROCK_DATA_SOURCE_KNOWLEDGE_BASE_ID_PROPERTY,
        ),
        resource
            .attributes
            .get(template::BEDROCK_DATA_SOURCE_ID_ATTRIBUTE)
            .cloned()
            .unwrap_or_default(),
    )
}

/// The NAME the data source is created under: the `Name` the declaration states, or — for a
/// declaration that states none, which that type's own contract marks required — the id
/// CloudFormation's naming rules computed for the resource, so the create carries a name that
/// member accepts rather than an empty one.
fn data_source_name(declared: &Value, resource: &StoredResource) -> String {
    declared
        .get(NAME_PROPERTY)
        .and_then(scalar_text)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| data_source_identity(resource).1)
}

/// One declared property document as the request members Amazon Bedrock Agent takes: every
/// member name re-cased ([`camel_cased`]), with the knowledge base left out — the URI carries
/// it, and a body member of that name would restate the label the request is already addressed
/// by — so one reader serves the create and the update alike.
fn declared_data_source_members(declared: &Value) -> Result<Map<String, Value>, Refusal> {
    let mut request = match camel_cased(declared, 0)? {
        Value::Object(members) => members,
        // A declaration that is not an object declares no member of the request.
        _ => Map::new(),
    };
    request.remove(&lower_initial(
        template::BEDROCK_DATA_SOURCE_KNOWLEDGE_BASE_ID_PROPERTY,
    ));
    Ok(request)
}

/// The attributes one answered data source reports ([`DATA_SOURCE_ATTRIBUTES`]).
///
/// An answer carrying no id is reported as a refusal rather than recorded as a created
/// resource: there would be no handle a client could call Amazon Bedrock Agent with, and
/// nothing for the stack to publish.
fn data_source_attributes(answered: &Value) -> Result<BTreeMap<String, String>, Refusal> {
    let Some(data_source) = answered.get(DATA_SOURCE_MEMBER) else {
        return Err(no_data_source_answered());
    };
    let attributes = reported_attributes(data_source, DATA_SOURCE_ATTRIBUTES);
    if !attributes.contains_key(template::BEDROCK_DATA_SOURCE_ID_ATTRIBUTE) {
        return Err(no_data_source_answered());
    }
    Ok(attributes)
}

/// The refusal an answer carrying no identified data source is reported as.
fn no_data_source_answered() -> Refusal {
    Refusal {
        code: "InternalFailure".to_string(),
        message: "Amazon Bedrock Agent answered a data source without an id".to_string(),
    }
}

/// Why a data source this stack created cannot settle: the status that service reports it in,
/// with the reasons it reports for it where there are any (`DataSource$failureReasons`).
fn failed_reason(physical_resource_id: &str, status: &str, data_source: &Value) -> String {
    let reasons: Vec<String> = data_source
        .get("failureReasons")
        .and_then(Value::as_array)
        .map(|reasons| reasons.iter().filter_map(scalar_text).collect())
        .unwrap_or_default();
    let reported = format!("Amazon Bedrock reports data source {physical_resource_id} as {status}");
    if reasons.is_empty() {
        return reported;
    }
    format!("{reported}: {}", reasons.join("; "))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a create this service refused because the knowledge base the declaration
    /// REFERENCES is not there is reported in the plugin's own not-found sentence, naming the
    /// referenced resource's CloudFormation type and the identifier the declaration stated;
    /// every other refusal is reported as that service worded it, and the code travels
    /// untouched either way.
    ///
    /// The rows differ along the identifier's FORM (the whole of the modelled
    /// `^[0-9a-zA-Z]{10}$` character set: digits, lower case, upper case and mixed) and along
    /// the CODE the refusal carried.
    #[test]
    fn a_missing_referenced_knowledge_base_is_reported_in_the_plugins_own_sentence() {
        let rows: &[(&str, &str, &str, &str, &str)] = &[
            (
                "a knowledge base of digits that service does not hold",
                BEDROCK_RESOURCE_NOT_FOUND,
                "KnowledgeBase with id 0123456789 is not found.",
                "0123456789",
                "Resource of type 'AWS::Bedrock::KnowledgeBase' with identifier '0123456789' \
                 was not found.",
            ),
            (
                "one of lower-case letters",
                BEDROCK_RESOURCE_NOT_FOUND,
                "KnowledgeBase with id abcdefghij is not found.",
                "abcdefghij",
                "Resource of type 'AWS::Bedrock::KnowledgeBase' with identifier 'abcdefghij' \
                 was not found.",
            ),
            (
                "one of upper-case letters",
                BEDROCK_RESOURCE_NOT_FOUND,
                "KnowledgeBase with id ABCDEFGHIJ is not found.",
                "ABCDEFGHIJ",
                "Resource of type 'AWS::Bedrock::KnowledgeBase' with identifier 'ABCDEFGHIJ' \
                 was not found.",
            ),
            (
                "one of mixed case and digits",
                BEDROCK_RESOURCE_NOT_FOUND,
                "KnowledgeBase with id aB3dE6gH9j is not found.",
                "aB3dE6gH9j",
                "Resource of type 'AWS::Bedrock::KnowledgeBase' with identifier 'aB3dE6gH9j' \
                 was not found.",
            ),
            (
                "a declaration that states no knowledge base at all",
                BEDROCK_RESOURCE_NOT_FOUND,
                "KnowledgeBase with id  is not found.",
                "",
                "Resource of type 'AWS::Bedrock::KnowledgeBase' with identifier '' was not \
                 found.",
            ),
            (
                "a create that service refused for the declaration itself",
                "ValidationException",
                "1 validation error detected",
                "aB3dE6gH9j",
                "1 validation error detected",
            ),
            (
                "a create that service would not serve at all",
                "AccessDeniedException",
                "User is not authorized to perform this action",
                "aB3dE6gH9j",
                "User is not authorized to perform this action",
            ),
            (
                "a name already in use",
                "ConflictException",
                "A data source with that name already exists.",
                "aB3dE6gH9j",
                "A data source with that name already exists.",
            ),
        ];
        for (case, code, message, knowledge_base_id, expected) in rows {
            let reported = referenced_knowledge_base_not_found(
                Refusal {
                    code: (*code).to_string(),
                    message: (*message).to_string(),
                },
                knowledge_base_id,
            );
            assert_eq!(
                reported.message, *expected,
                "case {case:?}: expected {expected:?}, got {:?}",
                reported.message
            );
            assert_eq!(
                reported.code, *code,
                "case {case:?}: expected the code {code:?} to travel untouched, got {:?}",
                reported.code
            );
        }
    }
}
