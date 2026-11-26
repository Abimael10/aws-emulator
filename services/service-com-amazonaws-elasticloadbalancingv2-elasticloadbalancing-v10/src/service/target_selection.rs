// SPDX-License-Identifier: CC0-1.0

//! WHICH registration of ONE target group a request a `forward` reaches, and the pin the
//! answer hands the client back.
//!
//! The target group this request was distributed to
//! ([`super::target_group_selection`]) holds the registrations the load balancer may send it
//! to, and the group's own attribute collection says how the load balancer chooses among
//! them:
//!
//! * `load_balancing.algorithm.type` — "[t]he load balancing algorithm determines how the
//!   load balancer selects targets when routing requests. The value is `round_robin`,
//!   `least_outstanding_requests`, or `weighted_random`. The default is `round_robin`". So a
//!   group of several targets spreads its requests over them: in ROTATION, by how many
//!   requests the load balancer currently has IN FLIGHT to each
//!   ([`crate::state::TargetDistribution`], which is the only thing either algorithm needs
//!   to remember), or by a draw per request.
//! * `stickiness.enabled` — "[i]ndicates whether target stickiness is enabled … The default
//!   is `false`" — with `stickiness.type` giving the kind: "`lb_cookie` and `app_cookie` for
//!   Application Load Balancers", "`source_ip` for Network Load Balancers",
//!   "`source_ip_dest_ip` and `source_ip_dest_ip_proto` for Gateway Load Balancers". Where it
//!   is on, "requests from a client should be routed to the same target" for the duration the
//!   group states — so the first request's choice is PINNED and every later request carrying
//!   that pin reaches the same registration.
//!
//! # What carries a pin
//!
//! A cookie-based pin travels in the load balancer's OWN cookies, the names
//! `stickiness.app_cookie.cookie_name` reserves: "[n]ames that start with the following
//! prefixes are not allowed: `AWSALB`, `AWSALBAPP`, and `AWSALBTG`; they're reserved for use
//! by the load balancer". [`DURATION_COOKIE`] and [`DURATION_CORS_COOKIE`] carry the
//! duration-based pin — the second so a cross-origin request carries it too, which is why it
//! states `SameSite=None`, and `Secure` beside it only where the listener the client dialled
//! terminates TLS — and [`APPLICATION_COOKIE`] carries the application-based one, which is
//! honored only while the request also carries the application's OWN cookie: that cookie is
//! what application-based stickiness follows, so a request without it is distributed like any
//! other.
//!
//! Each cookie carries a TOKEN of the registration ([`target_token`]) rather than the target
//! itself, exactly as the target-group pin carries a token of the group's ARN: a later
//! request is pinned by taking the token of each registration the group holds and comparing,
//! so the pin needs nothing stored, and nothing of the account's own naming — an instance id,
//! an address — travels to the client.
//!
//! A `source_ip` pin carries nothing at all: the "cookie" is the connection's own address, so
//! the choice is that address's own ([`distributed`]) and every request from it reaches the
//! same registration for as long as the group holds it.
//!
//! Nothing here reads or writes a store: it is handed what the group holds and answers which
//! position of it one request reaches.

use ::sha2::{Digest as _, Sha256};

use crate::state::{TargetGroupRecord, TargetKey};

use super::target_group_selection::carried_cookies;

/// The load balancer's own cookie carrying a duration-based pin.
pub(super) const DURATION_COOKIE: &str = "AWSALB";

/// The same pin for a client sending it cross-origin, which is what `SameSite=None` on it is
/// for.
pub(super) const DURATION_CORS_COOKIE: &str = "AWSALBCORS";

/// The load balancer's own cookie carrying an application-based pin, beside the
/// application's own cookie that pin follows.
pub(super) const APPLICATION_COOKIE: &str = "AWSALBAPP";

/// The value `load_balancing.algorithm.type` names, as the attribute's own value set spells
/// them.
const ROUND_ROBIN: &str = "round_robin";
const LEAST_OUTSTANDING_REQUESTS: &str = "least_outstanding_requests";
const WEIGHTED_RANDOM: &str = "weighted_random";

/// The `stickiness.type` values, likewise.
const LB_COOKIE: &str = "lb_cookie";
const APP_COOKIE: &str = "app_cookie";

/// The attribute keys this reading takes, each read through the group's own collection
/// (`TargetGroupRecord::effective_attribute`), so what the plane performs is what a read of
/// the group reports.
const ALGORITHM_KEY: &str = "load_balancing.algorithm.type";
const STICKINESS_ENABLED_KEY: &str = "stickiness.enabled";
const STICKINESS_TYPE_KEY: &str = "stickiness.type";
const APP_COOKIE_NAME_KEY: &str = "stickiness.app_cookie.cookie_name";
const APP_COOKIE_DURATION_KEY: &str = "stickiness.app_cookie.duration_seconds";
const LB_COOKIE_DURATION_KEY: &str = "stickiness.lb_cookie.duration_seconds";

/// How a load balancer selects the target of one request among the ones it may send it to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Algorithm {
    /// "round_robin", the default: the next request goes to the next target.
    RoundRobin,
    /// "least_outstanding_requests": the target the load balancer has the fewest requests in
    /// flight to.
    LeastOutstandingRequests,
    /// "weighted_random": a draw per request.
    WeightedRandom,
}

/// What pins a client to one registration of the group, where `stickiness.enabled` is on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Pin {
    /// `lb_cookie`: the load balancer's own cookies carry the pin, for the duration
    /// `stickiness.lb_cookie.duration_seconds` states.
    LoadBalancerCookie { duration_seconds: Option<i32> },
    /// `app_cookie`: the pin follows the APPLICATION's own cookie of that name, and travels
    /// beside it in the load balancer's own, for the duration
    /// `stickiness.app_cookie.duration_seconds` states.
    ApplicationCookie {
        cookie_name: String,
        duration_seconds: Option<i32>,
    },
    /// `source_ip` and the Gateway spellings: the connection's own address is the pin.
    SourceAddress,
}

/// How one target group distributes the requests it receives across its own registrations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Policy {
    pub(super) algorithm: Algorithm,
    /// What pins a client to one registration, and `None` for a group whose stickiness is
    /// off — the default — where every request is distributed on its own.
    pub(super) pin: Option<Pin>,
}

/// The policy `group` carries: its algorithm and its stickiness, read off its own attribute
/// collection.
pub(super) fn policy(group: &TargetGroupRecord) -> Policy {
    let attribute = |key: &str| group.effective_attribute(key);
    let seconds = |key: &str| attribute(key).and_then(|value| value.parse::<i32>().ok());
    let algorithm = match attribute(ALGORITHM_KEY).as_deref() {
        Some(LEAST_OUTSTANDING_REQUESTS) => Algorithm::LeastOutstandingRequests,
        Some(WEIGHTED_RANDOM) => Algorithm::WeightedRandom,
        // "The default is round_robin", which is also what a group carrying no such
        // attribute at all — one whose targets are not an instance or an address — spreads
        // its requests by.
        Some(ROUND_ROBIN) | Some(_) | None => Algorithm::RoundRobin,
    };
    let enabled = attribute(STICKINESS_ENABLED_KEY).as_deref() == Some("true");
    let pin = enabled.then(|| match attribute(STICKINESS_TYPE_KEY).as_deref() {
        Some(APP_COOKIE) => Pin::ApplicationCookie {
            cookie_name: attribute(APP_COOKIE_NAME_KEY).unwrap_or_default(),
            duration_seconds: seconds(APP_COOKIE_DURATION_KEY),
        },
        Some(LB_COOKIE) => Pin::LoadBalancerCookie {
            duration_seconds: seconds(LB_COOKIE_DURATION_KEY),
        },
        // Every other spelling is one of the address-based kinds, which carry no cookie.
        _ => Pin::SourceAddress,
    });
    Policy { algorithm, pin }
}

/// The token of one registration of `target_group_arn` the load balancer's own pin carries.
///
/// A digest of the group and the registration, so the cookie names the target to this
/// service — the token of each registration the group holds is taken and compared — and names
/// nothing of the account's own resources to the client holding it.
pub(super) fn target_token(target_group_arn: &str, key: &TargetKey) -> String {
    let (id, port) = key;
    let mut digest = Sha256::new();
    digest.update(target_group_arn.as_bytes());
    digest.update([0]);
    digest.update(id.as_bytes());
    digest.update([0]);
    digest.update(port.unwrap_or_default().to_string().as_bytes());
    ::hex::encode(digest.finalize())
}

/// The position of `tokens` this request is already PINNED to, read off the `Cookie` header
/// it carried — and `None` for a request carrying no pin of this group's kind, for a pin
/// naming a registration the group no longer holds, and for a group whose stickiness is off.
///
/// An application-based pin is honored only where the request ALSO carries the application's
/// own cookie: that cookie is what such stickiness follows, so a request without it carries
/// no pin to honor. An address-based pin carries nothing in the header at all — the choice
/// itself is the address's ([`distributed`]).
pub(super) fn pinned_at(
    policy: &Policy,
    tokens: &[String],
    cookie_header: Option<&str>,
) -> Option<usize> {
    let carried = |wanted: &[&str]| -> Option<String> {
        carried_cookies(cookie_header)
            .find(|(name, _)| wanted.contains(name))
            .map(|(_, value)| value.to_string())
    };
    let token = match policy.pin.as_ref()? {
        Pin::LoadBalancerCookie { .. } => carried(&[DURATION_COOKIE, DURATION_CORS_COOKIE])?,
        Pin::ApplicationCookie { cookie_name, .. } => {
            let name = cookie_name.trim();
            if name.is_empty() || carried(&[name]).is_none() {
                return None;
            }
            carried(&[APPLICATION_COOKIE])?
        }
        Pin::SourceAddress => return None,
    };
    tokens.iter().position(|held| *held == token)
}

/// The position of `count` registrations a request carrying NO pin reaches: the algorithm's
/// own choice, or — for an address-based pin — the one the connection's own address names.
///
/// `rotation` is where the group's own rotation has got to ([`crate::state::TargetDistribution`]),
/// `in_flight` how many requests the load balancer currently has in flight to each
/// registration in the same order, and `drawn` this request's draw on the unit interval.
///
/// `None` for a distribution over no registration at all. Every answer is a position of the
/// list, so a caller cannot be handed one the group does not hold.
pub(super) fn distributed(
    policy: &Policy,
    rotation: usize,
    in_flight: &[u32],
    drawn: f64,
    source_address: &str,
) -> Option<usize> {
    let count = in_flight.len();
    if count == 0 {
        return None;
    }
    // An address-based pin is not a choice among the targets at all: every request from one
    // address reaches the registration that address names, for as long as the group holds
    // it, and no algorithm is consulted.
    if policy.pin.as_ref() == Some(&Pin::SourceAddress) {
        return Some(address_position(source_address, count));
    }
    let from = rotation % count;
    match policy.algorithm {
        Algorithm::RoundRobin => Some(from),
        // The target the fewest requests are in flight to, and — among the targets that tie,
        // which is every target of a group nothing is in flight to — the next one in the
        // group's own rotation, so a group whose targets all answer at once still spreads its
        // requests over them.
        Algorithm::LeastOutstandingRequests => (0..count)
            .map(|step| (from + step) % count)
            .min_by_key(|at| in_flight[*at]),
        Algorithm::WeightedRandom => {
            // The registrations of one group carry no weights of their own — a weight belongs
            // to a target group of a forward action (`TargetGroupTuple$Weight`) — so the draw
            // is over the registrations evenly.
            let landed = (drawn.clamp(0.0, 1.0) * count as f64) as usize;
            Some(landed.min(count - 1))
        }
    }
}

/// The position of `count` registrations the connection address `source_address` names.
///
/// A digest of the address, so the choice is that address's own: it is the same on every
/// request from it (which is what an address-based pin IS) and two addresses are spread over
/// the registrations rather than landing on one.
fn address_position(source_address: &str, count: usize) -> usize {
    let digest = Sha256::digest(source_address.trim().as_bytes());
    let mut taken = [0_u8; 8];
    taken.copy_from_slice(&digest[..8]);
    usize::try_from(u64::from_be_bytes(taken) % count as u64).unwrap_or_default()
}

/// The `Set-Cookie` fields that hand the client the pin to the registration `token` names —
/// empty for a group whose stickiness is off and for one whose pin is the address itself,
/// which travels in no cookie.
///
/// `over_tls` is whether the listener the client dialled terminates TLS, which is what
/// decides whether the cross-origin cookie states `Secure` beside its `SameSite=None`: the
/// attribute states a fact about the connection the cookie travelled over, and a client
/// dialling a listener in the clear did not make a secure one.
pub(super) fn set_cookies(policy: &Policy, token: &str, over_tls: bool) -> Vec<String> {
    match policy.pin.as_ref() {
        Some(Pin::LoadBalancerCookie { duration_seconds }) => {
            let mut cors = field(DURATION_CORS_COOKIE, token, *duration_seconds);
            // The cross-origin copy is the one a client may send on a cross-site request,
            // which is what `SameSite=None` states; a browser takes that only on a secure
            // connection, so `Secure` accompanies it exactly where the connection was one.
            cors.push_str("; SameSite=None");
            if over_tls {
                cors.push_str("; Secure");
            }
            vec![field(DURATION_COOKIE, token, *duration_seconds), cors]
        }
        Some(Pin::ApplicationCookie {
            duration_seconds, ..
        }) => vec![field(APPLICATION_COOKIE, token, *duration_seconds)],
        Some(Pin::SourceAddress) | None => Vec::new(),
    }
}

/// One `Set-Cookie` field: the pin, for every path of the load balancer, lasting the duration
/// the group states — "[t]he time period, in seconds, during which requests from a client
/// should be routed to the same target".
fn field(name: &str, token: &str, duration_seconds: Option<i32>) -> String {
    let mut rendered = format!("{name}={token}; Path=/");
    if let Some(seconds) = duration_seconds.filter(|seconds| *seconds > 0) {
        rendered.push_str(&format!("; Max-Age={seconds}"));
    }
    rendered
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A target group of `family`'s protocol over `target_type` targets, carrying
    /// `attributes`.
    fn group(protocol: &str, target_type: &str, attributes: &[(&str, &str)]) -> TargetGroupRecord {
        TargetGroupRecord {
            arn: "arn:aws:elasticloadbalancing:us-west-2:111111111111:targetgroup/blue/1"
                .to_string(),
            protocol: Some(protocol.to_string()),
            target_type: target_type.to_string(),
            attributes: attributes
                .iter()
                .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
                .collect(),
            ..TargetGroupRecord::default()
        }
    }

    /// RULE: the policy is the group's own attribute collection — the algorithm it names and
    /// the stickiness it enables — with the documented default for every attribute nothing
    /// set.
    ///
    /// The rows span what the reading generalises over: each value of the algorithm
    /// attribute, stickiness off (the default) and on under each of its kinds, and a group
    /// the attribute family does not apply to at all.
    #[test]
    fn the_policy_is_the_groups_own_attribute_collection() {
        let rows: Vec<(&str, TargetGroupRecord, Policy)> = vec![
            (
                "a group nothing configured",
                group("HTTP", "instance", &[]),
                Policy {
                    algorithm: Algorithm::RoundRobin,
                    pin: None,
                },
            ),
            (
                "the least-outstanding-requests algorithm",
                group(
                    "HTTP",
                    "instance",
                    &[(ALGORITHM_KEY, LEAST_OUTSTANDING_REQUESTS)],
                ),
                Policy {
                    algorithm: Algorithm::LeastOutstandingRequests,
                    pin: None,
                },
            ),
            (
                "the weighted-random algorithm",
                group("HTTP", "ip", &[(ALGORITHM_KEY, WEIGHTED_RANDOM)]),
                Policy {
                    algorithm: Algorithm::WeightedRandom,
                    pin: None,
                },
            ),
            (
                "duration-based stickiness, by default for this family",
                group("HTTP", "instance", &[(STICKINESS_ENABLED_KEY, "true")]),
                Policy {
                    algorithm: Algorithm::RoundRobin,
                    pin: Some(Pin::LoadBalancerCookie {
                        duration_seconds: Some(86_400),
                    }),
                },
            ),
            (
                "duration-based stickiness for a stated duration",
                group(
                    "HTTP",
                    "instance",
                    &[
                        (STICKINESS_ENABLED_KEY, "true"),
                        (STICKINESS_TYPE_KEY, LB_COOKIE),
                        (LB_COOKIE_DURATION_KEY, "3600"),
                    ],
                ),
                Policy {
                    algorithm: Algorithm::RoundRobin,
                    pin: Some(Pin::LoadBalancerCookie {
                        duration_seconds: Some(3600),
                    }),
                },
            ),
            (
                "application-based stickiness",
                group(
                    "HTTP",
                    "instance",
                    &[
                        (STICKINESS_ENABLED_KEY, "true"),
                        (STICKINESS_TYPE_KEY, APP_COOKIE),
                        (APP_COOKIE_NAME_KEY, "SESSION"),
                        (APP_COOKIE_DURATION_KEY, "600"),
                    ],
                ),
                Policy {
                    algorithm: Algorithm::RoundRobin,
                    pin: Some(Pin::ApplicationCookie {
                        cookie_name: "SESSION".to_string(),
                        duration_seconds: Some(600),
                    }),
                },
            ),
            (
                "stickiness stated as off",
                group("HTTP", "instance", &[(STICKINESS_ENABLED_KEY, "false")]),
                Policy {
                    algorithm: Algorithm::RoundRobin,
                    pin: None,
                },
            ),
            (
                "a Network Load Balancer's group, whose stickiness is the address",
                group("TCP", "instance", &[(STICKINESS_ENABLED_KEY, "true")]),
                Policy {
                    algorithm: Algorithm::RoundRobin,
                    pin: Some(Pin::SourceAddress),
                },
            ),
        ];
        for (case, record, expected) in rows {
            let read = policy(&record);
            assert_eq!(
                read, expected,
                "case {case:?}: expected {expected:?}, got {read:?}"
            );
        }
    }

    /// What a row of the pin table states: the case, the group's policy, the `Cookie` header
    /// the request carried, and the position the pin names.
    type Pinned<'r> = (&'r str, &'r Policy, Option<String>, Option<usize>);

    /// RULE: a request carrying the load balancer's own pin reaches the registration that pin
    /// names, and a request carrying no pin this group honors reaches none.
    ///
    /// The rows span what the reading generalises over: each cookie a pin travels in, the
    /// application-based pin with and without the application's own cookie beside it, a pin
    /// naming a registration the group no longer holds, a group whose stickiness is off, and
    /// the address-based kind that carries no cookie at all.
    #[test]
    fn a_carried_pin_names_the_registration_it_was_minted_for() {
        let arn = "arn:aws:elasticloadbalancing:us-west-2:111111111111:targetgroup/blue/1";
        let tokens: Vec<String> = [("i-1", 80), ("i-2", 80)]
            .iter()
            .map(|(id, port)| target_token(arn, &((*id).to_string(), Some(*port))))
            .collect();
        let second = tokens[1].clone();
        let duration = Policy {
            algorithm: Algorithm::RoundRobin,
            pin: Some(Pin::LoadBalancerCookie {
                duration_seconds: Some(86_400),
            }),
        };
        let application = Policy {
            algorithm: Algorithm::RoundRobin,
            pin: Some(Pin::ApplicationCookie {
                cookie_name: "SESSION".to_string(),
                duration_seconds: Some(600),
            }),
        };
        let off = Policy {
            algorithm: Algorithm::RoundRobin,
            pin: None,
        };
        let address = Policy {
            algorithm: Algorithm::RoundRobin,
            pin: Some(Pin::SourceAddress),
        };
        let rows: Vec<Pinned<'_>> = vec![
            (
                "the duration cookie",
                &duration,
                Some(format!("{DURATION_COOKIE}={second}")),
                Some(1),
            ),
            (
                "the cross-origin copy of it",
                &duration,
                Some(format!("{DURATION_CORS_COOKIE}={second}")),
                Some(1),
            ),
            (
                "the duration cookie among others",
                &duration,
                Some(format!("first=1; {DURATION_COOKIE}={second}; last=2")),
                Some(1),
            ),
            ("no cookie at all", &duration, None, None),
            (
                "a pin naming a registration this group does not hold",
                &duration,
                Some(format!("{DURATION_COOKIE}=0123456789abcdef")),
                None,
            ),
            (
                "the application pin beside the application's own cookie",
                &application,
                Some(format!("SESSION=abc; {APPLICATION_COOKIE}={second}")),
                Some(1),
            ),
            (
                "the application pin without the application's own cookie",
                &application,
                Some(format!("{APPLICATION_COOKIE}={second}")),
                None,
            ),
            (
                "the application's own cookie without the pin",
                &application,
                Some("SESSION=abc".to_string()),
                None,
            ),
            (
                "a group whose stickiness is off",
                &off,
                Some(format!("{DURATION_COOKIE}={second}")),
                None,
            ),
            (
                "a group pinned by the address, which carries no cookie",
                &address,
                Some(format!("{DURATION_COOKIE}={second}")),
                None,
            ),
        ];
        for (case, policy, header, expected) in rows {
            let read = pinned_at(policy, &tokens, header.as_deref());
            assert_eq!(
                read, expected,
                "case {case:?}: expected {expected:?}, got {read:?}"
            );
        }
    }

    /// What a row of the distribution table states: the case, the group's algorithm, where its
    /// rotation has got to, the requests in flight to each registration, the row's own draw,
    /// and the position one request reaches.
    type Distributed<'r> = (&'r str, Algorithm, usize, &'r [u32], f64, Option<usize>);

    /// RULE: a request carrying no pin is distributed across the registrations the way the
    /// group's algorithm names — in rotation, by what is in flight, or by its own draw — and
    /// a group pinned by the address sends every request from one address to one
    /// registration.
    #[test]
    fn a_request_with_no_pin_is_distributed_the_way_the_algorithm_names() {
        let with = |algorithm: Algorithm| Policy {
            algorithm,
            pin: None,
        };
        let rows: &[Distributed<'_>] = &[
            ("the rotation's first position", Algorithm::RoundRobin, 0, &[0, 0, 0], 0.0, Some(0)),
            ("its second", Algorithm::RoundRobin, 1, &[0, 0, 0], 0.0, Some(1)),
            ("its third", Algorithm::RoundRobin, 2, &[0, 0, 0], 0.0, Some(2)),
            ("a rotation past the end, which begins again", Algorithm::RoundRobin, 4, &[0, 0, 0], 0.0, Some(1)),
            (
                "the fewest in flight",
                Algorithm::LeastOutstandingRequests,
                0,
                &[3, 1, 2],
                0.0,
                Some(1),
            ),
            (
                "the fewest in flight, wherever the rotation is",
                Algorithm::LeastOutstandingRequests,
                2,
                &[3, 1, 2],
                0.0,
                Some(1),
            ),
            (
                "a tie, broken by the rotation",
                Algorithm::LeastOutstandingRequests,
                1,
                &[0, 0, 0],
                0.0,
                Some(1),
            ),
            (
                "a tie among the two that are equal",
                Algorithm::LeastOutstandingRequests,
                1,
                &[0, 5, 0],
                0.0,
                Some(2),
            ),
            ("the lowest draw", Algorithm::WeightedRandom, 0, &[0, 0], 0.0, Some(0)),
            ("a draw below the boundary", Algorithm::WeightedRandom, 0, &[0, 0], 0.499, Some(0)),
            ("a draw at the boundary", Algorithm::WeightedRandom, 0, &[0, 0], 0.5, Some(1)),
            ("the highest draw", Algorithm::WeightedRandom, 0, &[0, 0], 1.0, Some(1)),
            ("no registration at all", Algorithm::RoundRobin, 0, &[], 0.0, None),
        ];
        for (case, algorithm, rotation, in_flight, drawn, expected) in rows {
            let chosen = distributed(&with(*algorithm), *rotation, in_flight, *drawn, "10.0.0.7");
            assert_eq!(
                chosen, *expected,
                "case {case:?}: expected {expected:?}, got {chosen:?}"
            );
        }

        // An address-based pin: one address always lands on the same registration, whatever
        // the rotation has got to, and the addresses of a range are spread over them.
        let address = Policy {
            algorithm: Algorithm::RoundRobin,
            pin: Some(Pin::SourceAddress),
        };
        let in_flight = [0_u32; 4];
        for rotation in 0..8 {
            let chosen = distributed(&address, rotation, &in_flight, 0.0, "10.0.0.7");
            assert_eq!(
                chosen,
                distributed(&address, 0, &in_flight, 0.9, "10.0.0.7"),
                "case \"one address at rotation {rotation}\": expected the same registration, \
                 got {chosen:?}"
            );
        }
        let landed: std::collections::BTreeSet<Option<usize>> = (1..40)
            .map(|host| distributed(&address, 0, &in_flight, 0.0, &format!("10.0.0.{host}")))
            .collect();
        assert!(
            landed.len() > 1,
            "case \"many addresses\": expected them spread over the registrations, all landed \
             on {landed:?}"
        );
    }

    /// RULE: the pin the answer hands back carries the registration's token under the load
    /// balancer's own cookie names, for the duration the group states, and carries nothing of
    /// the account's own naming; a group whose pin is the address hands back no cookie.
    #[test]
    fn the_minted_pin_carries_the_token_under_the_reserved_names() {
        let arn = "arn:aws:elasticloadbalancing:us-west-2:111111111111:targetgroup/blue/1";
        let token = target_token(arn, &("i-0abc".to_string(), Some(8080)));
        assert_eq!(
            token,
            target_token(arn, &("i-0abc".to_string(), Some(8080))),
            "case \"one registration\": expected one token"
        );
        assert_ne!(
            token,
            target_token(arn, &("i-0abc".to_string(), Some(8081))),
            "case \"the same target on another port\": expected another token"
        );
        assert!(
            !token.contains("i-0abc") && !token.contains("targetgroup"),
            "case \"the token\": expected it to carry nothing of the account's naming, got \
             {token:?}"
        );

        let duration = Policy {
            algorithm: Algorithm::RoundRobin,
            pin: Some(Pin::LoadBalancerCookie {
                duration_seconds: Some(3600),
            }),
        };
        let in_the_clear = set_cookies(&duration, &token, false);
        assert_eq!(
            in_the_clear.len(),
            2,
            "case \"duration-based stickiness\": expected both cookies, got {in_the_clear:?}"
        );
        assert!(
            in_the_clear[0].starts_with(&format!("{DURATION_COOKIE}={token}; Path=/"))
                && in_the_clear[0].contains("; Max-Age=3600"),
            "case \"the duration cookie\": got {:?}",
            in_the_clear[0]
        );
        assert!(
            in_the_clear[1].starts_with(&format!("{DURATION_CORS_COOKIE}={token}; Path=/"))
                && in_the_clear[1].contains("; SameSite=None")
                && !in_the_clear[1].contains("Secure"),
            "case \"the cross-origin cookie over a listener in the clear\": got {:?}",
            in_the_clear[1]
        );
        let over_tls = set_cookies(&duration, &token, true);
        assert!(
            over_tls[1].contains("; SameSite=None; Secure"),
            "case \"the cross-origin cookie over TLS\": got {:?}",
            over_tls[1]
        );

        let application = Policy {
            algorithm: Algorithm::RoundRobin,
            pin: Some(Pin::ApplicationCookie {
                cookie_name: "SESSION".to_string(),
                duration_seconds: Some(600),
            }),
        };
        let minted = set_cookies(&application, &token, false);
        assert_eq!(
            minted,
            vec![format!("{APPLICATION_COOKIE}={token}; Path=/; Max-Age=600")],
            "case \"application-based stickiness\": got {minted:?}"
        );

        for pin in [Some(Pin::SourceAddress), None] {
            let policy = Policy {
                algorithm: Algorithm::RoundRobin,
                pin,
            };
            let minted = set_cookies(&policy, &token, false);
            assert!(
                minted.is_empty(),
                "case \"a pin that travels in no cookie\": expected none, got {minted:?}"
            );
        }
    }
}
