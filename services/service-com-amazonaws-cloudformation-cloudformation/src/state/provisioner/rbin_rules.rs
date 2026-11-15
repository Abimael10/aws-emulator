// SPDX-License-Identifier: CC0-1.0

//! The per-type provisioning arms of Recycle Bin's own control plane: the retention rule an
//! `AWS::Rbin::Rule` declares. One file of [`super`]'s per-type arms, dispatched to by its
//! `Provisioner`.
//!
//! A retention rule is an account-wide guardrail rather than a named resource: `CreateRule`
//! takes the resource type to retain, the period to retain it for and the tags that select
//! what it covers, takes NO identifier at all, and answers with the eleven-character
//! `Identifier` Recycle Bin minted and the `RuleArn` built around it. That ARN is the type's
//! primary identifier — what its `Ref` returns — so it becomes the resource's physical id,
//! and the identifier inside it is the handle `GetRule`, `UpdateRule` and `DeleteRule` each
//! address one by ([`rule_identifier`]).
//!
//! The type's properties ARE `CreateRule`'s own members ([`RBIN_RULE_MEMBERS`]): `restJson1`
//! carries a member under its own name unless the model renames it, and that operation
//! renames none. What a rule REPORTS is that service's — the identifier it minted, the ARN it
//! built and the lock state it keeps — so each published value is read off its own answer
//! rather than computed here.

use super::*;

/// Recycle Bin's signing name, from its model (`sigv4Name: rbin`), which speaks `restJson1` —
/// so its calls ride on [`call_rest_json_named`], each on the URI its own operation's `@http`
/// binding declares, naming the operation so a refusal is reported the way that service's own
/// SDK raises it.
pub(super) const RBIN_SIGNING_NAME: &str = "rbin";

/// The collection `CreateRule` is bound to (`POST /rules`), and the collection one rule is
/// addressed under by the identifier that service minted (`GET`/`PATCH`/`DELETE
/// /rules/{Identifier}`).
const RULES_URI: &str = "/rules";

/// The collection a rule's TAGS are addressed under, by the rule's own ARN
/// (`/tags/{ResourceArn}`): `ListTagsForResource` reads them, `TagResource` writes them and
/// `UntagResource` drops them.
const TAGS_URI: &str = "/tags";

/// The operations this module calls, as Recycle Bin's own model names them.
const CREATE_RULE: &str = "CreateRule";
const GET_RULE: &str = "GetRule";
const UPDATE_RULE: &str = "UpdateRule";
const DELETE_RULE: &str = "DeleteRule";
const LIST_TAGS_FOR_RESOURCE: &str = "ListTagsForResource";
const TAG_RESOURCE: &str = "TagResource";
const UNTAG_RESOURCE: &str = "UntagResource";

/// The error code Recycle Bin answers for a rule it does not hold
/// (`ResourceNotFoundException`, the modelled 404 of its read, update and delete).
const RBIN_RESOURCE_NOT_FOUND: &str = "ResourceNotFoundException";

/// The member the tag set travels as on the create and on `TagResource` alike: a `TagList` of
/// `Tag` structures, whose members are spelled `Key`/`Value` ([`UPPER_CASED_TAG`]).
const TAGS_MEMBER: &str = "Tags";

/// The properties of the type that are members of `CreateRule`'s request body, each under the
/// member of the same name.
///
/// The tag set is not among them: it is merged with the tags CloudFormation propagates before
/// it is written ([`tags_of`]).
const RBIN_RULE_MEMBERS: &[(&str, &str)] = &[
    ("Description", "Description"),
    ("ExcludeResourceTags", "ExcludeResourceTags"),
    ("LockConfiguration", "LockConfiguration"),
    ("ResourceTags", "ResourceTags"),
    ("ResourceType", "ResourceType"),
    ("RetentionPeriod", "RetentionPeriod"),
];

/// The members `UpdateRule` takes of those: that call "can update a retention rule's
/// description, resource tags, and retention period", and its `ResourceType` member is
/// documented "currently not supported. You can't update a retention rule's resource type
/// after creation" — which is why the type is this type's one replacement property
/// ([`super::template::replacement_properties`]) rather than a member of this request.
///
/// The LOCK is not among them either: Recycle Bin puts a lock on a rule, and takes one off,
/// through `LockRule`/`UnlockRule` rather than through `UpdateRule`, so a declaration that
/// changes it is refused rather than reported as applied ([`update_rbin_rule`]).
const RBIN_RULE_UPDATABLE_MEMBERS: &[(&str, &str)] = &[
    ("Description", "Description"),
    ("ExcludeResourceTags", "ExcludeResourceTags"),
    ("ResourceTags", "ResourceTags"),
    ("RetentionPeriod", "RetentionPeriod"),
];

/// Every property of the type this arm carries onto the owning API, so a declaration stating
/// one outside the set is REFUSED rather than reported as created without it
/// ([`refuse_unmapped_properties`]): the create's own members above, and the tag set.
const RBIN_RULE_MAPPED_PROPERTIES: &[&str] = &[
    "Description",
    "ExcludeResourceTags",
    "LockConfiguration",
    "ResourceTags",
    "ResourceType",
    "RetentionPeriod",
    "Tags",
];

/// The property Recycle Bin fixes when it makes the rule, which `UpdateRule` cannot carry: the
/// lock a rule holds.
const RBIN_RULE_LOCK_PROPERTY: &str = "LockConfiguration";

/// The structures a numeric member of the request is nested in, and the member inside each
/// that is a NUMBER on the wire: `RetentionPeriod$RetentionPeriodValue` (`@range 1..3650`) and
/// `LockConfiguration$UnlockDelay$UnlockDelayValue`, from that service's own model
/// (`smithy_query(query="shape", shape="RetentionPeriod,LockConfiguration,UnlockDelay")`).
///
/// A template states a value either in JSON's own spelling or as the TEXT a parameter
/// reference resolves to — `Ref` on a parameter answers that parameter's value, which a stack
/// operation carries as a string whatever the parameter's declared type — so each is read in
/// the one spelling the member takes ([`declared_number`]) before the request is sent.
const RETENTION_PERIOD_MEMBER: &str = "RetentionPeriod";
const RETENTION_PERIOD_VALUE_MEMBER: &str = "RetentionPeriodValue";
const UNLOCK_DELAY_MEMBER: &str = "UnlockDelay";
const UNLOCK_DELAY_VALUE_MEMBER: &str = "UnlockDelayValue";

/// The members Recycle Bin reports a rule's identity in: the eleven-character identifier it
/// minted and the ARN it built around it.
const IDENTIFIER_MEMBER: &str = "Identifier";
const RULE_ARN_MEMBER: &str = "RuleArn";

/// The member it reports a rule's lock state in, which a rule that has NEVER been locked is
/// reported without at all: "null - The retention rule has never been locked"
/// (`LockState`'s own documentation).
const LOCK_STATE_MEMBER: &str = "LockState";

/// The lock state this type publishes for a rule that has never been locked: that rule is one
/// "unlocked and it can be modified or deleted by any user with the required permissions",
/// which is the `unlocked` member of `LockState`'s own value set — so the attribute carries a
/// value for every rule rather than being absent for a rule nobody ever locked.
const LOCK_STATE_UNLOCKED: &str = "unlocked";

/// The lock state a rule created WITH a lock configuration carries, from that same value set.
const LOCK_STATE_LOCKED: &str = "locked";

/// The scalar properties `GetRule` reports back under the names the type declares, for drift
/// detection: the rule's description and the resource type it retains. A property that is a
/// STRUCTURE or a LIST on the wire (the retention period, either selection) is left out, which
/// is the position every arm whose property is not a scalar is in and what [`drift`]
/// documents.
const RBIN_RULE_REPORTED_PROPERTIES: &[(&str, &str)] =
    &[("Description", "Description"), ("ResourceType", "ResourceType")];

/// Create the retention rule an `AWS::Rbin::Rule` declares, in Recycle Bin — so a client that
/// deployed the stack reads the rule back through that service's own `GetRule` and `ListRules`
/// for the identity the stack published, and the resources the rule covers really are retained
/// on deletion.
///
/// The declaration IS the request ([`RBIN_RULE_MEMBERS`]), with the declared tags beside the
/// ones CloudFormation gives the resource ([`tags_of`]) as the `TagList` that call's own `Tags`
/// member takes.
///
/// The identity is Recycle Bin's: the `RuleArn` it answered becomes this resource's physical
/// id — the type's primary identifier, which its `Ref` returns — and the identifier and the
/// lock state it reported are the attributes the type publishes beside it.
pub(super) async fn create_rbin_rule(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    refuse_unmapped_properties(resource, RBIN_RULE_MAPPED_PROPERTIES)?;
    let properties = resource
        .properties
        .as_object()
        .cloned()
        .unwrap_or_else(Map::new);
    let mut request = Map::new();
    copy_members(&properties, RBIN_RULE_MEMBERS, &mut request);
    carried_as(&mut request);
    let tags = tags_of(&resource.properties, propagated);
    if !tags.is_empty() {
        request.insert(
            TAGS_MEMBER.to_string(),
            tags_as_list(&tags, UPPER_CASED_TAG),
        );
    }
    let answered = call_rest_json_named(
        ctx,
        RBIN_SIGNING_NAME,
        region,
        CREATE_RULE,
        "POST",
        RULES_URI,
        Some(&Value::Object(request)),
    )
    .await?;
    let arn = reported_text(&answered, RULE_ARN_MEMBER).unwrap_or_default();
    if arn.is_empty() {
        // There would be no handle a client could call Recycle Bin with, and nothing for the
        // stack to publish, so this is a failed create rather than a created resource.
        return Err(Refusal {
            code: "InternalFailure".to_string(),
            message: "the Recycle Bin control plane answered a created retention rule without \
                      an ARN"
                .to_string(),
        });
    }
    Ok(Created {
        physical_resource_id: Some(arn.clone()),
        attributes: reported_rule_attributes(Some(&answered), &arn),
    })
}

/// Apply a changed declaration to the rule the stack created, through `UpdateRule` — the one
/// call that reconfigures a rule that exists.
///
/// That call takes the rule's description, its retention period and either resource selection
/// ([`RBIN_RULE_UPDATABLE_MEMBERS`]), addressed by the identifier Recycle Bin minted. The
/// resource TYPE is not one of them, which is why a declaration restating it replaces the rule
/// instead of reaching here ([`super::template::replacement_properties`]).
///
/// A declaration that changes the LOCK is refused rather than quietly dropped: Recycle Bin
/// locks and unlocks a rule through `LockRule`/`UnlockRule`, calls this emulator does not make
/// on a stack's behalf, so the rule is left as it is and the deployment reports why. The tags
/// travel separately ([`retag_rbin_rule`]).
pub(super) async fn update_rbin_rule(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Created, Refusal> {
    if resource.properties.get(RBIN_RULE_LOCK_PROPERTY) != declared.get(RBIN_RULE_LOCK_PROPERTY) {
        return Err(Refusal {
            code: NOT_IMPLEMENTED.to_string(),
            message: format!(
                "this emulator does not change the {RBIN_RULE_LOCK_PROPERTY} property of an \
                 existing {}, which Recycle Bin answers through its own LockRule and \
                 UnlockRule; the resource was left as it is.",
                resource.resource_type
            ),
        });
    }
    let properties = declared.as_object().cloned().unwrap_or_else(Map::new);
    let mut request = Map::new();
    copy_members(&properties, RBIN_RULE_UPDATABLE_MEMBERS, &mut request);
    carried_as(&mut request);
    let answered = call_rest_json_named(
        ctx,
        RBIN_SIGNING_NAME,
        region,
        UPDATE_RULE,
        "PATCH",
        &rule_uri(&resource.physical_resource_id),
        Some(&Value::Object(request)),
    )
    .await?;
    Ok(Created::attributes(reported_rule_attributes(
        Some(&answered),
        &resource.physical_resource_id,
    )))
}

/// Bring the tags of the rule a resource record names to the ones this deployment gives it,
/// through Recycle Bin's own `TagResource` and `UntagResource`.
///
/// The set is the declaration's own `Tags` beside the ones CloudFormation propagates
/// ([`tags_of`]) — the same set the CREATE applied — so a key outside it is one this deployment
/// no longer gives the resource and is removed, and one in it with another value is re-valued.
/// Where the two already agree nothing is called at all, which is what makes this safe to ask
/// on every deployment; a rule that service no longer holds has no tags to reconcile.
///
/// Those calls address a rule by its ARN, which is this resource's physical id.
pub(super) async fn retag_rbin_rule(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
    propagated: &[(String, String)],
) -> Result<(), Refusal> {
    let uri = format!(
        "{TAGS_URI}/{}",
        percent_encoded(&resource.physical_resource_id)
    );
    let answered = match call_rest_json_named(
        ctx,
        RBIN_SIGNING_NAME,
        region,
        LIST_TAGS_FOR_RESOURCE,
        "GET",
        &uri,
        None,
    )
    .await
    {
        Ok(answered) => answered,
        Err(refusal) if refusal.code == RBIN_RESOURCE_NOT_FOUND => return Ok(()),
        Err(refusal) => return Err(refusal),
    };
    // That service answers a rule's tags as the `[{Key, Value}]` list a declaration's `Tags`
    // property is also written as, so one reader serves both sides.
    let held = declared_tags(&answered);
    let desired = tags_of(declared, propagated);
    let (added, removed) = tag_delta(&held, &desired);
    if !added.is_empty() {
        call_rest_json_named(
            ctx,
            RBIN_SIGNING_NAME,
            region,
            TAG_RESOURCE,
            "POST",
            &uri,
            Some(&Value::Object(
                [(
                    TAGS_MEMBER.to_string(),
                    tags_as_list(&added, UPPER_CASED_TAG),
                )]
                .into_iter()
                .collect(),
            )),
        )
        .await?;
    }
    if !removed.is_empty() {
        // `tagKeys` is an `@httpQuery` LIST, which travels as the parameter repeated once per
        // key rather than as one joined value.
        let keys = removed
            .iter()
            .map(|key| format!("tagKeys={}", percent_encoded(key)))
            .collect::<Vec<String>>()
            .join("&");
        call_rest_json_named(
            ctx,
            RBIN_SIGNING_NAME,
            region,
            UNTAG_RESOURCE,
            "DELETE",
            &format!("{uri}?{keys}"),
            None,
        )
        .await?;
    }
    Ok(())
}

/// Delete the retention rule a stack created, addressed by the identifier Recycle Bin minted
/// for it — the one inside this resource's physical id. A rule it no longer holds
/// (`ResourceNotFoundException`, judged on the CODE and never on a message that carries the
/// request's own identifier) is already in the state the delete asks for.
pub(super) async fn delete_rbin_rule(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<(), Refusal> {
    match call_rest_json_named(
        ctx,
        RBIN_SIGNING_NAME,
        region,
        DELETE_RULE,
        "DELETE",
        &rule_uri(physical_resource_id),
        None,
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == RBIN_RESOURCE_NOT_FOUND => Ok(()),
        Err(refusal) => Err(refusal),
    }
}

/// The rule's configuration as Recycle Bin reports it, for drift detection: `GetRule`'s own
/// record, read by the identifier inside the ARN the stack published. One that service no
/// longer holds was deleted.
pub(super) async fn describe_rbin_rule(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    Ok(read_rbin_rule(ctx, region, physical_resource_id)
        .await?
        .map(|rule| reported_as_properties(&rule, RBIN_RULE_REPORTED_PROPERTIES)))
}

/// The rule record that service holds under the identifier inside `physical_resource_id`, or
/// `None` when it holds none (`ResourceNotFoundException`, judged on the CODE and never on a
/// message).
async fn read_rbin_rule(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    match call_rest_json_named(
        ctx,
        RBIN_SIGNING_NAME,
        region,
        GET_RULE,
        "GET",
        &rule_uri(physical_resource_id),
        None,
    )
    .await
    {
        Ok(answered) => Ok(Some(answered)),
        Err(refusal) if refusal.code == RBIN_RESOURCE_NOT_FOUND => Ok(None),
        Err(refusal) => Err(refusal),
    }
}

/// The attributes one reported rule gives the resource: the ARN it is addressed by, the
/// identifier Recycle Bin minted and the lock state it keeps.
///
/// A rule that has never been locked is reported with NO lock state at all — "null - The
/// retention rule has never been locked" — and this type publishes a value for every rule, so
/// that state is reported as the `unlocked` one it denotes ([`LOCK_STATE_UNLOCKED`]). The
/// identifier falls back to the one inside the ARN, which is where every call on a rule reads
/// it from.
fn reported_rule_attributes(rule: Option<&Value>, arn: &str) -> BTreeMap<String, String> {
    let mut attributes = BTreeMap::new();
    attributes.insert("Arn".to_string(), arn.to_string());
    let identifier = rule
        .and_then(|rule| reported_text(rule, IDENTIFIER_MEMBER))
        .unwrap_or_else(|| rule_identifier(arn).to_string());
    attributes.insert(IDENTIFIER_MEMBER.to_string(), identifier);
    let lock_state = rule
        .and_then(|rule| reported_text(rule, LOCK_STATE_MEMBER))
        .unwrap_or_else(|| LOCK_STATE_UNLOCKED.to_string());
    attributes.insert(LOCK_STATE_MEMBER.to_string(), lock_state);
    attributes
}

/// The URI one rule is addressed at: the collection and the `Identifier` `@httpLabel` escaped
/// as a single path segment ([`percent_encoded`]), so a value outside that member's own pattern
/// is refused by that service's own validation rather than addressing another path.
fn rule_uri(physical_resource_id: &str) -> String {
    format!(
        "{RULES_URI}/{}",
        percent_encoded(rule_identifier(physical_resource_id))
    )
}

/// The identifier Recycle Bin minted for the rule a physical id names: the last segment of the
/// `RuleArn` the create answered, whose resource path is `rule/<identifier>`
/// (`RuleArn`'s own pattern,
/// `^arn:\S+:rbin:[a-z\-0-9]{0,63}:[0-9]{12}:rule/[0-9a-zA-Z]{11}…$`). An id that carries no
/// segment separator at all is taken as the identifier itself, so nothing is addressed under
/// an empty label.
fn rule_identifier(physical_resource_id: &str) -> &str {
    physical_resource_id
        .split('/')
        .next_back()
        .unwrap_or(physical_resource_id)
}

/// The lock state a DECLARATION asks for, before Recycle Bin has reported one: a rule declared
/// with a lock configuration is created `locked` — that is what `CreateRule`'s own
/// `LockConfiguration` member does — and one declared without it has never been locked, which
/// this type publishes as [`LOCK_STATE_UNLOCKED`].
///
/// Read by the return-value catalogue, which resolves a declaration's `Fn::GetAtt` before
/// anything exists; the deployment then re-points the attribute at what that service reported
/// ([`reported_rule_attributes`]).
pub(in crate::state) fn declared_lock_state(properties: &Value) -> &'static str {
    if properties.get(RBIN_RULE_LOCK_PROPERTY).is_some() {
        LOCK_STATE_LOCKED
    } else {
        LOCK_STATE_UNLOCKED
    }
}

/// Put the two numeric members of the request into the spelling they are carried in
/// ([`RETENTION_PERIOD_VALUE_MEMBER`], [`UNLOCK_DELAY_VALUE_MEMBER`]).
///
/// A value already in that spelling is left exactly as it came, and so is one that is neither
/// spelling of a number: the owning operation's own validation is what refuses a value that is
/// no number at all, rather than this dropping it.
fn carried_as(request: &mut Map<String, Value>) {
    numeric_member(
        request.get_mut(RETENTION_PERIOD_MEMBER),
        RETENTION_PERIOD_VALUE_MEMBER,
    );
    numeric_member(
        request
            .get_mut(RBIN_RULE_LOCK_PROPERTY)
            .and_then(|lock| lock.get_mut(UNLOCK_DELAY_MEMBER)),
        UNLOCK_DELAY_VALUE_MEMBER,
    );
}

/// Read `member` of the structure `holder` in the one spelling a number is carried in
/// ([`declared_number`]). A holder the declaration did not state, or one that is no structure
/// at all, carries no member to convert.
fn numeric_member(holder: Option<&mut Value>, member: &str) {
    let Some(Value::Object(members)) = holder else {
        return;
    };
    let Some(converted) = members.get(member).and_then(declared_number) else {
        return;
    };
    members.insert(member.to_string(), converted);
}
