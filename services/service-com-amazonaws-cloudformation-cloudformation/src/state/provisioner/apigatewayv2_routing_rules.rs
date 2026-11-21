// SPDX-License-Identifier: CC0-1.0

//! The Amazon API Gateway v2 ROUTING RULE arms: the rule an `AWS::ApiGatewayV2::RoutingRule`
//! declares on a custom domain name, created in the service that owns it so a client's own
//! `GetRoutingRule` and `ListRoutingRules` resolve what the stack publishes. One file of
//! [`super`]'s per-type arms, dispatched to by its `Provisioner`.
//!
//! A routing rule is a CHILD of a custom domain name — every operation on one is bound under
//! that domain name's own URI (`/v2/domainnames/{DomainName}/routingrules`, plus the rule's
//! own id for the read, the put and the delete) — and the declaration names that domain by
//! its ARN (`DomainNameArn`) rather than by the name those URIs take. So the name is read out
//! of the ARN ([`routing_rule_domain_name`]), the one reading of that this crate has: both
//! ARNs this type works with carry the domain name in the same place, the custom domain
//! name's own and the rule's.
//!
//! The rule's identity is that service's. `CreateRoutingRule` takes no identifier for the
//! rule at all — the declaration states its priority, the conditions it matches on and the
//! actions it takes — and answers the `routingRuleId` that service minted together with the
//! `routingRuleArn` it built around it. That ARN is the type's primary identifier, so it is
//! this resource's physical id and what `Ref` resolves to, and the id travels beside it as
//! the type's other published return value.
//!
//! `PutRoutingRule` is the one call that changes a rule that exists: it puts the whole rule
//! under the id it already has, taking exactly the three members the create does, so a
//! changed declaration of any of them is applied to the rule the stack created rather than
//! replacing it. The DOMAIN NAME is not one of them — it addresses the rule rather than being
//! a value of it, and that service publishes no call that moves a rule to another domain — so
//! a declaration naming another domain asks for a different rule and the deployment replaces
//! the resource (`crate::state::template::replacement_properties` names that property for
//! this type).

use super::*;

/// The property a declaration names the custom domain name the rule lives on by: its ARN,
/// which is what the type publishes rather than the name every routing-rule URI takes.
///
/// Visible to the rest of this service's state because the return-value catalogue reads the
/// same property to compute the stand-in identity of a rule that does not exist yet.
pub(in crate::state) const ROUTING_RULE_DOMAIN_NAME_ARN: &str = "DomainNameArn";

/// The two values this type publishes through `Fn::GetAtt`: the ARN API Gateway v2 built for
/// the rule — the type's primary identifier — and the id it minted.
///
/// Visible to the rest of this service's state for the reason the property above is: the
/// catalogue that computes a stand-in for each of them names them here.
pub(in crate::state) const ROUTING_RULE_ARN_ATTRIBUTE: &str = "RoutingRuleArn";
pub(in crate::state) const ROUTING_RULE_ID_ATTRIBUTE: &str = "RoutingRuleId";

/// The members the create and the put answer those two values in.
const ROUTING_RULE_ARN_MEMBER: &str = "routingRuleArn";
const ROUTING_RULE_ID_MEMBER: &str = "routingRuleId";

/// The last segment of the collection one custom domain name's routing rules live in, as the
/// `@http` uri of every routing-rule operation spells it.
const ROUTING_RULES_SEGMENT: &str = "routingrules";

/// Where a custom domain NAME sits in the resource path of an ARN of this control plane, and
/// where a routing rule's own id sits in a routing rule's.
///
/// API Gateway's resource portion for a custom domain name is `/domainnames/{name}`, and a
/// rule of that domain name nests under it (`/domainnames/{name}/routingrules/{id}`) — so
/// these two are what an ARN of either resource is read at.
const DOMAIN_NAMES_ARN_SEGMENT: &str = "/domainnames/";
const ROUTING_RULES_ARN_SEGMENT: &str = "/routingrules/";

/// The custom domain NAME an ARN of this control plane names: the text its resource path
/// carries after `/domainnames/`, up to the next `/`.
///
/// One reading for both ARNs that name a routing rule's domain — the domain name's own, which
/// a declaration states, and the rule's, which carries the domain it was created on — so the
/// create cannot address a different domain name from the read, the put and the delete.
///
/// Text that carries no such resource path yields the EMPTY name, which addresses no domain
/// name at all: a call made with it is refused by the service that owns the resource rather
/// than sent to another one ([`routing_rule_uri`] refuses it before any call is made).
pub(in crate::state) fn routing_rule_domain_name(arn: &str) -> &str {
    arn.split_once(DOMAIN_NAMES_ARN_SEGMENT)
        .map_or("", |(_, resource)| {
            resource.split('/').next().unwrap_or_default()
        })
}

/// The rule's own id a routing-rule ARN carries: the text after `/routingrules/`, which is
/// the last segment of that resource path.
fn routing_rule_id(arn: &str) -> &str {
    arn.split_once(ROUTING_RULES_ARN_SEGMENT)
        .map_or("", |(_, id)| id)
}

/// The URI of the routing-rule collection of ONE custom domain name, which is the create's
/// own binding.
fn routing_rules_uri(domain_name: &str) -> String {
    format!(
        "{APIGATEWAYV2_DOMAIN_NAMES_URI}/{}/{ROUTING_RULES_SEGMENT}",
        percent_encoded(domain_name)
    )
}

/// The URI that addresses ONE routing rule — the binding every operation on a rule that
/// exists is bound to — built from the ARN that service minted, which is this resource's
/// physical id: that ARN carries both of those operations' `@httpLabel`s, the domain name the
/// rule lives on and the id it was minted under.
///
/// An id that carries neither is REFUSED rather than sent as a URI that addresses something
/// else: nothing identifies the rule to read, change or delete, so the resource's operation
/// fails instead of reporting a success about another resource.
fn routing_rule_uri(resource: &StoredResource) -> Result<String, Refusal> {
    let identity = resource.physical_resource_id.as_str();
    let domain_name = routing_rule_domain_name(identity);
    let rule_id = routing_rule_id(identity);
    if domain_name.is_empty() || rule_id.is_empty() {
        return Err(Refusal {
            code: MISSING_PROPERTY_CODE.to_string(),
            message: format!(
                "the identity recorded for this {} names no routing rule of a custom domain \
                 name",
                resource.resource_type
            ),
        });
    }
    Ok(format!(
        "{}/{}",
        routing_rules_uri(domain_name),
        percent_encoded(rule_id)
    ))
}

/// One declared routing rule as the request that creates or puts it: every property but the
/// domain name's ARN, which addresses the collection rather than travelling as a member, each
/// under the member name that model spells it with — the property with its initial letter
/// lower-cased, all the way down ([`lower_camel_members`]), which is what turns a declared
/// `Conditions: [{MatchHeaders: {AnyOf: [{Header, ValueGlob}]}}]` into that list's own
/// members.
fn routing_rule_request(declared: &Value) -> Result<Value, Refusal> {
    let mut request = Map::new();
    if let Some(properties) = declared.as_object() {
        for (property, value) in properties {
            if property == ROUTING_RULE_DOMAIN_NAME_ARN {
                continue;
            }
            request.insert(lower_initial(property), lower_camel_members(value, 0)?);
        }
    }
    Ok(Value::Object(request))
}

/// What an answered create or put reports about the rule: the ARN API Gateway v2 built for it
/// — this resource's identity, which is what the type's `Ref` returns — and the id it minted,
/// both read off what the call ANSWERED rather than computed here.
///
/// A call that answered 2xx without either leaves nothing that addresses the rule, so it is
/// reported as a failed resource rather than recorded under an identity nothing minted
/// ([`api_gateway_answered`]).
fn routing_rule_created(answered: &Value, operation: &str) -> Result<Created, Refusal> {
    let arn = api_gateway_answered(answered, ROUTING_RULE_ARN_MEMBER, operation)?;
    let rule_id = api_gateway_answered(answered, ROUTING_RULE_ID_MEMBER, operation)?;
    let mut attributes = BTreeMap::new();
    attributes.insert(ROUTING_RULE_ARN_ATTRIBUTE.to_string(), arn.clone());
    attributes.insert(ROUTING_RULE_ID_ATTRIBUTE.to_string(), rule_id);
    Ok(Created {
        physical_resource_id: Some(arn),
        attributes,
    })
}

/// Create the routing rule an `AWS::ApiGatewayV2::RoutingRule` declares, so a client that
/// deployed the stack reads the rule back through `apigatewayv2 GetRoutingRule` and
/// `ListRoutingRules` on the custom domain name it was declared for — and that domain really
/// routes the requests the rule's conditions match to the api its action names — rather than a
/// domain name holding no rule at all.
///
/// `CreateRoutingRule` (`POST /v2/domainnames/{DomainName}/routingrules`) is bound to the
/// domain name the declaration's `DomainNameArn` names, and every other property of the type
/// is a member of it ([`routing_rule_request`]). A domain name that service does not hold is
/// its own `NotFoundException`, which fails this resource rather than publishing a rule
/// nothing holds.
pub(in crate::state) async fn create_apigatewayv2_routing_rule(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<Created, Refusal> {
    let domain_name_arn = required_property(resource, ROUTING_RULE_DOMAIN_NAME_ARN)?;
    let request = routing_rule_request(&resource.properties)?;
    let answered = call_rest_json(
        ctx,
        API_GATEWAY_SIGNING_NAME,
        region,
        "POST",
        &routing_rules_uri(routing_rule_domain_name(&domain_name_arn)),
        Some(&request),
    )
    .await?;
    routing_rule_created(&answered, "CreateRoutingRule")
}

/// Apply a changed declaration to the routing rule the stack created, so a client that edits
/// a rule's priority, its conditions or its actions through CloudFormation gets the rule it
/// already has changed — under the same id and ARN API Gateway v2 minted for it — rather than
/// a deployment that refuses the change.
///
/// `PutRoutingRule` (`PUT /v2/domainnames/{DomainName}/routingrules/{RoutingRuleId}`) "puts" a
/// whole rule under the id it addresses, and takes exactly the members the create does — so
/// the request is the NEW declaration in full rather than the properties that changed: a
/// member a put omits is a member the rule would no longer have, and all three are required
/// of it.
pub(in crate::state) async fn update_apigatewayv2_routing_rule(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Created, Refusal> {
    let uri = routing_rule_uri(resource)?;
    let request = routing_rule_request(declared)?;
    let answered = call_rest_json(
        ctx,
        API_GATEWAY_SIGNING_NAME,
        region,
        "PUT",
        &uri,
        Some(&request),
    )
    .await?;
    routing_rule_created(&answered, "PutRoutingRule")
}

/// Delete the routing rule a stack created, addressed exactly as its own read is. One API
/// Gateway v2 no longer holds (`NotFoundException`, judged on the CODE) is already in the
/// state the delete asks for — which is also what deleting the custom domain name that held
/// it leaves behind, since a domain name owns its routing rules.
pub(in crate::state) async fn delete_apigatewayv2_routing_rule(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<(), Refusal> {
    let uri = routing_rule_uri(resource)?;
    api_gateway_deleted(ctx, region, &uri).await
}

/// The rule's configuration as API Gateway v2 reports it, for drift detection:
/// `GetRoutingRule`'s own record, read by the same ARN the delete addresses the rule by. One
/// that service no longer holds (`NotFoundException`) was deleted.
///
/// The one property it reports as a SCALAR is the rule's priority; its conditions and its
/// actions are LISTS on the wire, so they take no part in the comparison — the position every
/// arm whose property is not a scalar is in, and what `super::drift` documents.
pub(in crate::state) async fn describe_apigatewayv2_routing_rule(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<Option<Value>, Refusal> {
    let uri = routing_rule_uri(resource)?;
    match call_rest_json(ctx, API_GATEWAY_SIGNING_NAME, region, "GET", &uri, None).await {
        Ok(answer) => Ok(Some(reported_as_properties(
            &answer,
            &[("Priority", "priority")],
        ))),
        Err(refusal) if refusal.code == API_GATEWAY_NOT_FOUND => Ok(None),
        Err(refusal) => Err(refusal),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The domain name and the rule id are read out of the ARNs that carry them, wherever the
    /// account field is written and whichever of the two resources the ARN names — and text
    /// that names neither reads as empty rather than as some other resource.
    #[test]
    fn an_arn_is_read_for_the_resources_its_path_names() {
        let cases: &[(&str, &str, &str)] = &[
            (
                "arn:aws:apigateway:us-east-1::/domainnames/api.example.com",
                "api.example.com",
                "",
            ),
            (
                "arn:aws:apigateway:us-east-1:000000000000:/domainnames/api.example.com\
                 /routingrules/abc123",
                "api.example.com",
                "abc123",
            ),
            (
                "arn:aws:apigateway:eu-west-2:000000000000:/domainnames/orders.example.org\
                 /routingrules/zz9999",
                "orders.example.org",
                "zz9999",
            ),
            ("arn:aws:apigateway:us-east-1::/apis/a1b2c3", "", ""),
            ("not-an-arn", "", ""),
            ("", "", ""),
        ];
        for (arn, domain_name, rule_id) in cases {
            assert_eq!(
                routing_rule_domain_name(arn),
                *domain_name,
                "case {arn:?}: expected the domain name {domain_name:?}, got {:?}",
                routing_rule_domain_name(arn)
            );
            assert_eq!(
                routing_rule_id(arn),
                *rule_id,
                "case {arn:?}: expected the rule id {rule_id:?}, got {:?}",
                routing_rule_id(arn)
            );
        }
    }

    /// The declaration is the request once its member names are re-cased, with the domain
    /// name's ARN left out: it is the collection the create is bound to, not a member of it.
    #[test]
    fn the_declaration_is_the_request_without_the_domain_name() {
        let declared = ::serde_json::json!({
            "DomainNameArn": "arn:aws:apigateway:us-east-1::/domainnames/api.example.com",
            "Priority": 10,
            "Conditions": [{ "MatchBasePaths": { "AnyOf": ["/orders"] } }],
            "Actions": [{ "InvokeApi": { "ApiId": "a1b2c3", "Stage": "prod" } }],
        });
        let request = routing_rule_request(&declared).expect("a request");
        assert_eq!(
            request,
            ::serde_json::json!({
                "priority": 10,
                "conditions": [{ "matchBasePaths": { "anyOf": ["/orders"] } }],
                "actions": [{ "invokeApi": { "apiId": "a1b2c3", "stage": "prod" } }],
            }),
            "case \"a declared rule\": expected the re-cased members of the create, got \
             {request}"
        );
    }
}
