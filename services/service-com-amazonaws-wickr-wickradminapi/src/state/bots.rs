// SPDX-License-Identifier: CC0-1.0

//! The bot half of this service's store.
//!
//! A bot is an account of ONE network, like a user, so it is keyed by
//! `networkId` here and goes with the network when that network is deleted. The
//! record carries every member of the modelled `Bot` shape, so the operation that
//! creates a bot, the one that reads it back and the one that lists them share a
//! single record and a single view of it.

/// The largest `botId` the modelled `BotId` (len 1..10, `^[0-9]+$`) can express.
const LAST_BOT_ID: u64 = 9_999_999_999;

/// `BotStatus.PENDING` (1): "The bot has been created but is not yet active,
/// pending initial setup or activation."
///
/// The other value `BotStatus` declares, ACTIVE (2), is what a bot reaches once
/// the bot software signs in with the challenge it was created with. That sign-in
/// is not an operation of this admin API and nothing else can stand in for it, so
/// a bot this slice stores waits in this status — which is also why it reports no
/// `uname`, no `lastLogin` and no `pubkey`.
pub const BOT_STATUS_PENDING: i32 = 1;

/// `BotStatus.ACTIVE` (2): "The bot is active and operational, able to send and
/// receive messages."
///
/// A bot reaches it by signing in, which is what produces the three values a bot
/// only has once its software has run: its username hash, its key pair and the
/// instant of that sign-in (see [`BotRecord::has_signed_in`]).
pub const BOT_STATUS_ACTIVE: i32 = 2;

/// What every bot account's `username` ends with.
///
/// `CreateBotRequest$username` "must be unique within the network and follow the
/// network's naming conventions", and the convention a bot account's name
/// follows is this suffix: a name that does not end with it is not a name a bot
/// of a network may hold. The model expresses no part of it — `GenericString`'s
/// `^[\S\s]*$` matches every string — so the service owns the whole rule and
/// this is RECORDED behaviour.
pub const BOT_USERNAME_SUFFIX: &str = "bot";

/// Why a bot `username` was refused: it does not end with [`BOT_USERNAME_SUFFIX`].
///
/// ENTIRELY fixed text, like the other reasons this service states: it names
/// neither the submitted name nor the network, so the refusal carries no part of
/// the request. Shared, so no operation that accepts a bot username can word the
/// convention or its refusal differently.
pub const BOT_USERNAME_INVALID_REASON: &str = "Bot Username must end with bot";

/// Whether `username` is a name a bot of a network may be created with.
///
/// The convention is the suffix and nothing else: the name has to END with
/// [`BOT_USERNAME_SUFFIX`], whatever the rest of it is built from and however
/// long it is. The suffix is matched without regard to case, which is the
/// reading that refuses nothing the recorded wire shows accepted — the recording
/// carries only lower-case names, so a name whose suffix differs only in case is
/// not known to be refused.
pub fn bot_username_follows_convention(username: &str) -> bool {
    username
        .len()
        .checked_sub(BOT_USERNAME_SUFFIX.len())
        // A split that does not land on a character boundary is not a suffix:
        // `get` answers `None` for it rather than cutting a character in half.
        .and_then(|start| username.get(start..))
        .is_some_and(|tail| tail.eq_ignore_ascii_case(BOT_USERNAME_SUFFIX))
}

/// One stored bot of one network — every member of the modelled `Bot` shape.
#[derive(Debug, Clone)]
pub struct BotRecord {
    pub bot_id: String,
    pub username: String,
    /// The username hash the bot is known by ONCE it has signed in — derived when
    /// the record is created, but reported only while [`Self::has_signed_in`]
    /// holds (see [`bot_view`]).
    pub uname: String,
    pub display_name: Option<String>,
    pub group_id: String,
    pub status: i32,
    pub suspended: bool,
    /// Whether the bot has a password set — what `Bot$hasChallenge` reports.
    pub has_challenge: bool,
}

impl BotRecord {
    /// Whether the bot software has signed in, which is what `status` reports: a
    /// bot is created PENDING and reaches [`BOT_STATUS_ACTIVE`] by signing in.
    ///
    /// The sign-in is what mints the values a bot carries only afterwards, so this
    /// is the one condition every member of that set is reported under.
    pub fn has_signed_in(&self) -> bool {
        self.status == BOT_STATUS_ACTIVE
    }
}

impl super::ComAmazonawsWickrWickradminapiState {
    /// Mint the next unused `botId`, or `None` once the id space is exhausted.
    ///
    /// Monotonic and never reset, so a deleted bot's id is never handed out
    /// again, and minted from one upwards so an id is always inside the 1..10
    /// digits `BotId` allows.
    pub fn mint_bot_id(&mut self) -> Option<String> {
        let next = self.minted_bots.checked_add(1)?;
        if next > LAST_BOT_ID {
            return None;
        }
        self.minted_bots = next;
        Some(next.to_string())
    }

    /// Store `record` under `network_id`.
    pub fn insert_bot(&mut self, network_id: &str, record: BotRecord) {
        self.bots
            .entry(network_id.to_string())
            .or_default()
            .insert(record.bot_id.clone(), record);
    }

    /// The bots of `network_id`, in `botId` order.
    pub fn bots_of(&self, network_id: &str) -> Vec<BotRecord> {
        self.bots
            .get(network_id)
            .map(|bots| bots.values().cloned().collect())
            .unwrap_or_default()
    }

    /// The bot of `network_id` carrying `bot_id`, if one does.
    ///
    /// Reporting only — a lookup, never a write.
    pub fn bot_of(&self, network_id: &str, bot_id: &str) -> Option<&BotRecord> {
        self.bots.get(network_id)?.get(bot_id)
    }

    /// The bot of `network_id` carrying `bot_id`, to CHANGE.
    ///
    /// The one way a stored bot is mutated, so an operation that changes a bot's
    /// properties writes to the very record every reporting operation answers
    /// from and cannot leave a second copy behind.
    pub fn bot_of_mut(&mut self, network_id: &str, bot_id: &str) -> Option<&mut BotRecord> {
        self.bots.get_mut(network_id)?.get_mut(bot_id)
    }

    /// Drop the bot of `network_id` carrying `bot_id`, answering the record that
    /// was removed, or `None` when that network holds no such bot.
    ///
    /// The bot account goes entirely: nothing of it is kept, so afterwards a read
    /// of it is a read of a bot that "has been deleted", it is counted in no
    /// listing and in no group's member tally, and the `username` it held is
    /// available to a new account of the network again. Only the minted-id
    /// counter outlives it ([`Self::mint_bot_id`] is monotonic), so the id is
    /// never handed out a second time.
    pub fn remove_bot(&mut self, network_id: &str, bot_id: &str) -> Option<BotRecord> {
        let bots = self.bots.get_mut(network_id)?;
        let removed = bots.remove(bot_id)?;
        // A network whose last bot is gone holds no bot map either, so the store
        // carries nothing for it that a later reader could see.
        if bots.is_empty() {
            self.bots.remove(network_id);
        }
        Some(removed)
    }

    /// The bot of `network_id` holding `username`, if one does.
    pub fn bot_by_username(&self, network_id: &str, username: &str) -> Option<&BotRecord> {
        self.bots
            .get(network_id)?
            .values()
            .find(|bot| bot.username == username)
    }

    /// How many bots of `network_id` are members of `group_id`.
    ///
    /// What `SecurityGroup$botMembers` ("the number of bot members") reports:
    /// derived from the bots stored under the network on every read, never cached
    /// on the group.
    pub fn bot_members_of(&self, network_id: &str, group_id: &str) -> usize {
        self.bots.get(network_id).map_or(0, |bots| {
            bots.values().filter(|bot| bot.group_id == group_id).count()
        })
    }
}

/// One stored bot as the modelled `Bot` shape.
///
/// Shared by every operation that reports a bot, so they cannot disagree about
/// which members a bot carries.
pub fn bot_view(record: &BotRecord) -> crate::ops::types::Bot {
    crate::ops::types::Bot {
        bot_id: Some(record.bot_id.clone()),
        display_name: record.display_name.clone(),
        username: Some(record.username.clone()),
        // The three values a bot holds only once its software has signed in: the
        // username hash it is then known by, the public key of the key pair it
        // generates then, and the instant of that sign-in. A bot that has not
        // signed in has none of them, so a read reports none of them for it.
        // Nothing in this slice signs a bot in, so the pubkey and the last login
        // are never known here, while the hash — derived from the network and the
        // username, so computable before the fact — stays on the record and is
        // reported only once the bot has signed in.
        uname: record.has_signed_in().then(|| record.uname.clone()),
        pubkey: None,
        last_login: None,
        status: Some(record.status),
        group_id: Some(record.group_id.clone()),
        has_challenge: Some(record.has_challenge),
        suspended: Some(record.suspended),
    }
}
