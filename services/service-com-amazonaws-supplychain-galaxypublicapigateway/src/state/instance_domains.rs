// SPDX-License-Identifier: CC0-1.0

//! The ADDRESS an AWS Supply Chain instance's web app and Identity-Center portal are
//! ANNOUNCED at, and the WebApp domain LABEL a create that named none is answered with.
//!
//! RULE: a host this API reports is a host a client RESOLVES and then dials, so an instance's
//! announced hostnames are the address THIS emulator answers on
//! ([`announced_instance_endpoint`]) — the `host[:port]` the request being answered reached
//! it at — and never a name in a zone AWS publishes its own hosts in. A name in AWS's zone
//! resolves nowhere near this process or, worse, at genuine AWS infrastructure, so a client
//! that follows the announced domain (a browser hand-off, a redirect, a health probe) would
//! leave the emulated environment carrying the credentials it is holding. Two deployments of
//! this emulator answer on two addresses, so they announce two hosts.
//!
//! A client configured with ONE local endpoint resolves nothing else — it cannot resolve a
//! per-instance name of this emulator's invention, and nothing answers for a sub-name of the
//! host this process is reached at — so the announced hosts carry the address and the
//! instance's own identity stays where this API already carries it: in `webAppDnsDomain`,
//! the member that holds the instance's domain LABEL, and in `instanceId`.
//!
//! The LABEL and the instance ID are two different identities here: `Instance$instanceId`
//! targets the model's `UUID` shape while `Instance$webAppDnsDomain` targets
//! `InstanceWebAppDnsDomain` (`@pattern ^(?![-])[a-zA-Z0-9-]{1,62}[a-zA-Z0-9]$`), documented
//! as "The DNS subdomain of the web app. This would be \"example\" in the URL
//! \"example.scn.global.on.aws\"" — a service-generated DNS label a caller may also choose
//! for itself. So a create that named no label is answered one MINTED here
//! ([`minted_web_app_dns_domain`]): a DNS label drawn independently of the instance's id and
//! of every label a live instance already holds, never the id itself.
//!
//! Nothing here is ever CONNECTED to: the module formats strings into a response and holds
//! no outbound client, socket or resolver.

use super::ID_MINT_ATTEMPTS;

/// The `host[:port]` an instance's web app and Identity-Center portal are announced at, for
/// the request `ctx` is serving: the address that request reached this emulator at.
///
/// In the order the other announced addresses of this tree read it:
///
/// * a request that addressed the EMULATOR'S OWN endpoint
///   ([`::service_common::data_plane_addressing::reached_emulator_endpoint_of_parts`]) comes
///   from a client that resolves nothing else, so that is the address it reached;
/// * a request on an emulator that IS served on a port
///   ([`::service_common::data_plane_addressing::served_endpoint_authority`]) — a caller that
///   addressed one of AWS's own hosts, an in-process call of another service included — is
///   answered the one address this process answers on;
/// * an emulator served on no port at all has one base and that base is read, so the value
///   still names this emulator rather than a host belonging to someone else.
#[must_use]
pub fn announced_instance_endpoint(ctx: &::smithy_server_runtime::OperationContext<'_>) -> String {
    let base = ctx.endpoint();
    ::service_common::data_plane_addressing::reached_emulator_endpoint_of_parts(ctx.parts())
        .or_else(|| {
            ::service_common::data_plane_addressing::served_endpoint_authority(base.as_str())
        })
        .unwrap_or_else(|| {
            ::service_common::data_plane_addressing::url_authority(base.as_str())
                .trim_end_matches('/')
                .to_string()
        })
}

/// The fewest characters `InstanceWebAppDnsDomain`'s modelled `@pattern`
/// (`^(?![-])[a-zA-Z0-9-]{1,62}[a-zA-Z0-9]$`) admits: one character of the leading class and
/// the alphanumeric the value ends on.
pub const MIN_WEB_APP_DNS_DOMAIN_LENGTH: usize = 2;

/// The most characters that same pattern admits: 62 of the leading class plus the final
/// alphanumeric.
pub const MAX_WEB_APP_DNS_DOMAIN_LENGTH: usize = 63;

/// Whether `value` has the form the model gives `InstanceWebAppDnsDomain`:
/// [`MIN_WEB_APP_DNS_DOMAIN_LENGTH`]..=[`MAX_WEB_APP_DNS_DOMAIN_LENGTH`] characters of ASCII
/// letters, digits and hyphens, not starting with a hyphen and ending on an alphanumeric.
///
/// Stated once, here, so what this service MINTS for the member and anything else that has
/// to recognise such a label cannot drift from the modelled constraint. The modelled
/// constraints already refuse a bad `webAppDnsDomain` MEMBER before a handler runs; this is
/// for the value the service generates itself, which no constraint check sees.
#[must_use]
pub fn is_modelled_web_app_dns_domain(value: &str) -> bool {
    (MIN_WEB_APP_DNS_DOMAIN_LENGTH..=MAX_WEB_APP_DNS_DOMAIN_LENGTH).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        && !value.starts_with('-')
        && value.ends_with(|character: char| character.is_ascii_alphanumeric())
}

/// How many characters a minted WebApp domain label carries: wide enough that no two
/// creates of one deployment draw one label, and well inside
/// [`MAX_WEB_APP_DNS_DOMAIN_LENGTH`].
const MINTED_LABEL_LENGTH: usize = 16;

/// One freshly MINTED WebApp domain label, for a create that named none:
/// [`MINTED_LABEL_LENGTH`] characters of the digits and letters a DNS label is written with
/// — a label of [`is_modelled_web_app_dns_domain`]'s form, and a label rather than an
/// identifier.
///
/// The VALUE is the emulator's own — AWS's is a per-run server-generated token, so no value
/// reproduces it — and what it has to satisfy is the member's modelled form and its
/// invariants: it is drawn independently of the instance's `instanceId`, which is a
/// different identity of a different modelled shape, and it is drawn afresh per create, so
/// the label a client keys a URL, a DNS record or a uniqueness check on is a label and not
/// an id.
///
/// The draw is the same v4 UUID the rest of this store's minted values come from, written in
/// the digits-and-letters alphabet through the tree's one mapping for that
/// ([`::service_common::minted_base36`]) rather than a second one here, and lower-cased
/// because a DNS label is written in lower case — so the result carries none of a UUID's
/// shape (its hyphen groups, and its hexadecimal alphabet).
#[must_use]
pub fn minted_web_app_dns_domain() -> String {
    ::service_common::minted_base36(&super::new_uuid_v4(), MINTED_LABEL_LENGTH)
        .to_ascii_lowercase()
}

impl super::ComAmazonawsSupplychainGalaxypublicapigatewayState {
    /// A WebApp domain label no live instance holds, for a create that named none.
    ///
    /// Minted labels are drawn until one satisfies the member's modelled form
    /// ([`is_modelled_web_app_dns_domain`], checked here because the value is the service's
    /// own and no constraint check sees it) and is free — the member is unique across live
    /// instances, "You can set this to a custom value, as long as the domain isn't already
    /// being used by someone else" (the member's own documentation), which is also what the
    /// create refuses a caller-chosen duplicate with. `None` when [`ID_MINT_ATTEMPTS`] draws
    /// collided — which cannot happen in practice, so the caller answers it with a modelled
    /// server error rather than spinning under the state lock, exactly as
    /// [`Self::fresh_instance_id`] does.
    pub fn fresh_web_app_dns_domain(&self) -> Option<String> {
        (0..ID_MINT_ATTEMPTS)
            .map(|_| minted_web_app_dns_domain())
            .find(|candidate| {
                is_modelled_web_app_dns_domain(candidate)
                    && self.web_app_dns_domain_holder(candidate).is_none()
            })
    }

    /// The live instance holding the WebApp domain label `domain`, or `None` when the label
    /// is free.
    ///
    /// One resolution for both sides of the member's uniqueness: the create refuses a
    /// caller-chosen label this answers an instance for, and
    /// [`Self::fresh_web_app_dns_domain`] draws again for a minted one, so the two cannot
    /// disagree about which labels are taken.
    pub fn web_app_dns_domain_holder(&self, domain: &str) -> Option<&super::InstanceRecord> {
        self.instances
            .values()
            .find(|record| record.web_app_dns_domain.as_deref() == Some(domain))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a minted label satisfies the member's modelled pattern and is a LABEL rather
    /// than an identifier — written in the digits and lower-case letters a DNS label
    /// carries, with none of a UUID's shape — and it is drawn afresh, so two creates never
    /// collide.
    #[test]
    fn a_minted_label_is_a_modelled_label_and_a_fresh_one() {
        let mut seen = ::std::collections::BTreeSet::new();
        for draw in 0..64 {
            let label = minted_web_app_dns_domain();
            assert!(
                is_modelled_web_app_dns_domain(&label),
                "case \"draw {draw}\": expected a label of the modelled form, got {label:?}"
            );
            assert_eq!(
                label.len(),
                MINTED_LABEL_LENGTH,
                "case \"draw {draw}\": expected {MINTED_LABEL_LENGTH} characters, got {label:?}"
            );
            assert!(
                label
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit()),
                "case \"draw {draw}\": expected digits and lower-case letters, got {label:?}"
            );
            assert!(
                !crate::state::is_modelled_uuid(&label),
                "case \"draw {draw}\": expected a DNS label rather than a UUID, got {label:?}"
            );
            assert!(
                seen.insert(label.clone()),
                "case \"draw {draw}\": expected a fresh label, got {label:?} twice"
            );
        }
    }

    /// RULE: the modelled form admits a label on either published bound and refuses one
    /// outside it, or one carrying a character the pattern's class does not.
    #[test]
    fn the_modelled_label_form_follows_the_published_pattern() {
        let longest = format!("{}z", "l".repeat(MAX_WEB_APP_DNS_DOMAIN_LENGTH - 1));
        let too_long = format!("{}z", "l".repeat(MAX_WEB_APP_DNS_DOMAIN_LENGTH));
        // Rows on each published bound, one past each, and one per character class the
        // pattern rules out.
        let cases: [(&str, &str, bool); 10] = [
            ("the shortest label", "a1", true),
            ("one character short of it", "a", false),
            ("the longest label", longest.as_str(), true),
            ("one character past it", too_long.as_str(), false),
            ("a hyphenated label", "demo-two-parts", true),
            ("mixed case and digits", "MixedCase9", true),
            ("a leading hyphen", "-demo", false),
            ("a trailing hyphen", "demo-", false),
            ("a dot", "demo.two", false),
            ("nothing at all", "", false),
        ];
        for (case, value, expected) in cases {
            let actual = is_modelled_web_app_dns_domain(value);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected} for {value:?}, got {actual}"
            );
        }
    }
}
