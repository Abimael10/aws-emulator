// SPDX-License-Identifier: CC0-1.0

//! How the `AWS::Route53Resolver::FirewallRuleGroupAssociation` provider words a refusal of
//! the Route 53 Resolver call it made, which is the message CloudFormation quotes on the
//! failed resource's event ([`crate::state::provisioning::handler_failure_reason`]).
//!
//! That provider is written against the CloudFormation resource-provider plugin and raises its
//! invalid-request exception around the REQUEST it issued rather than around the service's own
//! diagnostic, so a refusal of the request reaches the reason as `Invalid request provided: `
//! followed by the rendering of that request ([`super::rendered_firewall_request`]). The
//! service's own sentence — its coded text and the per-request trace clause it closes with —
//! is no part of that message, so what a client reads is the call the handler made and the
//! values it made it with, the same string on every attempt at the same deployment.
//!
//! A refusal of any OTHER class is passed on as that service worded it: the plugin's wording
//! is the wording of the invalid-request exception alone, and the class is decided by the
//! error CODE the owning service answered ([`crate::state::provisioning::handler_error_code`])
//! — never by message text a request can influence — so this wording and the
//! `HandlerErrorCode` the event carries cannot disagree.
//!
//! Which of the two a type is, is that type's provider's own and is stated per type, as the
//! rest of the per-type half of provisioning is: the DNS Firewall domain list's and the rule
//! group's providers report the service's message as it came — a rule group whose declared rule
//! that service will not make carries that service's own coded sentence on its event, which the
//! cross-service suite
//! `core/router/tests/all/cloudformation_provisions_route53resolver_firewall.rs` holds them to —
//! so this wording applies to the ASSOCIATION's calls ([`call_firewall_association`]) and not
//! at [`super::call_route53resolver`], which every `AWS::Route53Resolver::*` arm shares.

use super::*;

/// Call one Route 53 Resolver operation for an
/// `AWS::Route53Resolver::FirewallRuleGroupAssociation`, and report a refusal of it as the
/// exception THAT type's provider raises for it ([`provider_exception`]).
///
/// Every call that type's arms make goes through here rather than through
/// [`super::call_route53resolver`], so the associate, the disassociate and the read a drift
/// detection makes all report a refused request the same way.
pub(crate) async fn call_firewall_association(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    operation: &str,
    request: &Value,
) -> Result<Value, Refusal> {
    call_route53resolver(ctx, region, operation, request)
        .await
        .map_err(|refusal| provider_exception(operation, request, refusal))
}

/// One refusal Route 53 Resolver answered, as the EXCEPTION the association's provider raises
/// for it: the plugin's invalid-request wording around the rendering of the request for a
/// refusal of the REQUEST, and the service's own message for every other class.
///
/// The CODE is carried through untouched: it is what decides the `HandlerErrorCode` the event
/// reports and what the arms of [`super`] read to tell an association that is already gone
/// from a refusal they must report.
fn provider_exception(operation: &str, request: &Value, refusal: Refusal) -> Refusal {
    if crate::state::provisioning::handler_error_code(&refusal.code)
        != crate::state::provisioning::HANDLER_ERROR_INVALID_REQUEST
    {
        return refusal;
    }
    match rendered_firewall_request(operation, request) {
        Some(rendered) => Refusal {
            message: format!(
                "{}{rendered}",
                crate::state::provisioner::INVALID_REQUEST_PROVIDED
            ),
            code: refusal.code,
        },
        None => refusal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only a refusal of the REQUEST is reworded: every other class reaches the event as the
    /// owning service worded it, and the code is carried through either way.
    #[test]
    fn only_a_refused_request_carries_the_plugin_wording() {
        let served = "[RSLVR-00000] The association is under mutation protection. \
                      Trace Id: \"1-00000000-000000000000000000000000\"";
        let cases: &[(&str, &str, &str)] = &[
            (
                "a validation refusal is the plugin's invalid-request exception",
                "ValidationException",
                "Invalid request provided: DisassociateFirewallRuleGroupRequest(\
                 FirewallRuleGroupAssociationId=rslvr-frgassoc-0a1b)",
            ),
            (
                "a state transition that service will not make",
                "ConflictException",
                served,
            ),
            ("a resource it does not hold", "ResourceNotFoundException", served),
            ("a throttled call", "ThrottlingException", served),
            ("a call the caller may not make", "AccessDeniedException", served),
        ];
        let request = serde_json::json!({
            "FirewallRuleGroupAssociationId": "rslvr-frgassoc-0a1b",
        });
        for (case, code, expected) in cases {
            let reported = provider_exception(
                "DisassociateFirewallRuleGroup",
                &request,
                Refusal {
                    code: (*code).to_string(),
                    message: served.to_string(),
                },
            );
            assert_eq!(
                reported.message, *expected,
                "case {case:?}: expected the message {expected:?}, got {:?}",
                reported.message
            );
            assert_eq!(
                reported.code, *code,
                "case {case:?}: expected the code {code:?}, got {:?}",
                reported.code
            );
        }
    }
}
