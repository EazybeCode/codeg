//! The stable reasons Neev declines a Teams request, and what the person is
//! told.
//!
//! Every refusal the bot or tab can produce is one of these codes; the text is
//! the ONLY thing a Teams user sees. It names what an operator must change, never
//! an internal detail (no org ids, no database errors, no token claims). The
//! codes themselves are logged and are stable API: tests pin them.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Refusal {
    /// The activity was addressed to a different bot registration.
    WrongBot,
    /// The sender's tenant is not connected to a Neev organization.
    UnmappedTenant,
    /// An administrator suspended Neev for this tenant, Team or channel.
    Suspended,
    /// The sender carries no Entra object id (guest, anonymous, federated).
    GuestSender,
    /// The sender's Entra object id is not linked to a Neev account.
    UnmappedUser,
    /// A channel mention from a channel Neev is not enabled in.
    UnmappedChannel,
    /// A channel message whose thread root cannot be identified.
    UnknownThread,
    /// Group chats are never mapped to a project.
    GroupChat,
    /// The mapped Neev account is not (or no longer) an active org member.
    NotAMember,
    /// The Neev chat backing this conversation could not be opened.
    SessionUnavailable,
    /// The conversation already has a reply in progress.
    Busy,
    /// The person is sending faster than Neev accepts.
    Limited,
    /// No agent runtime is bound on this server.
    RuntimeUnavailable,
    /// Anything else the service refused; retrying may help.
    Unavailable,
    /// A card action Neev does not recognise, or malformed action data.
    UnknownAction,
    /// A card action naming a run this person cannot see.
    RunNotFound,
    /// Teams membership, guest status, Team ownership or channel audience
    /// could not be VERIFIED (no Graph authority, consent missing or revoked,
    /// Team unknown to the Bot Connector). Fails closed.
    TeamsUnverifiable,
    /// The channel is not a live standard channel free of guests and members
    /// from other tenants.
    ChannelNotEligible,
    /// The sender is not (or no longer) a member of the Team.
    NotATeamMember,
    /// Linking a Team requires a verified owner of that Team.
    NotTeamOwner,
    /// Only the person a run executes as can stop it.
    NotRunOwner,
}

impl Refusal {
    #[cfg(test)]
    pub(crate) const ALL: [Refusal; 21] = [
        Self::WrongBot,
        Self::UnmappedTenant,
        Self::Suspended,
        Self::GuestSender,
        Self::UnmappedUser,
        Self::UnmappedChannel,
        Self::UnknownThread,
        Self::GroupChat,
        Self::NotAMember,
        Self::SessionUnavailable,
        Self::Busy,
        Self::Limited,
        Self::RuntimeUnavailable,
        Self::Unavailable,
        Self::UnknownAction,
        Self::RunNotFound,
        Self::TeamsUnverifiable,
        Self::ChannelNotEligible,
        Self::NotATeamMember,
        Self::NotTeamOwner,
        Self::NotRunOwner,
    ];

    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::WrongBot => "teams_wrong_bot",
            Self::UnmappedTenant => "teams_unmapped_tenant",
            Self::Suspended => "teams_suspended",
            Self::GuestSender => "teams_guest_sender",
            Self::UnmappedUser => "teams_unmapped_user",
            Self::UnmappedChannel => "teams_unmapped_channel",
            Self::UnknownThread => "teams_unknown_thread",
            Self::GroupChat => "teams_group_chat",
            Self::NotAMember => "teams_not_a_member",
            Self::SessionUnavailable => "teams_session_unavailable",
            Self::Busy => "teams_busy",
            Self::Limited => "teams_limited",
            Self::RuntimeUnavailable => "teams_runtime_unavailable",
            Self::Unavailable => "teams_unavailable",
            Self::UnknownAction => "teams_unknown_action",
            Self::RunNotFound => "teams_run_not_found",
            Self::TeamsUnverifiable => "teams_unverifiable",
            Self::ChannelNotEligible => "teams_channel_not_eligible",
            Self::NotATeamMember => "teams_not_team_member",
            Self::NotTeamOwner => "teams_not_team_owner",
            Self::NotRunOwner => "teams_not_run_owner",
        }
    }

    pub(crate) fn message(self) -> &'static str {
        match self {
            Self::WrongBot => "This message was not addressed to Neev.",
            Self::UnmappedTenant => {
                "Your Microsoft 365 organization is not connected to Neev. Ask your Neev administrator to add it."
            }
            Self::GuestSender => {
                "Neev only answers members of a connected organization. Guest and external accounts cannot use it."
            }
            Self::Suspended => {
                "Neev is turned off here by your Neev administrator."
            }
            Self::UnmappedUser => {
                "Your Teams account is not linked to a Neev account yet. Open the Neev tab in Teams while signed in to Neev to link it."
            }
            Self::UnmappedChannel => {
                "Neev is not enabled in this channel. Ask your Neev administrator to enable it."
            }
            Self::UnknownThread => {
                "Neev could not tell which conversation this message belongs to. Mention Neev in a new post or a reply to one."
            }
            Self::GroupChat => "Neev is not available in group chats. Message Neev directly or mention it in a connected channel.",
            Self::NotAMember => {
                "Your linked Neev account is not an active member of this organization."
            }
            Self::SessionUnavailable => "Neev could not open the chat for this conversation. Try again shortly.",
            Self::Busy => "Neev is still working on your previous message in this conversation.",
            Self::Limited => "You are sending messages too quickly. Wait a moment and try again.",
            Self::RuntimeUnavailable => "Agent runs are not enabled on this Neev server.",
            Self::Unavailable => "Neev is temporarily unavailable. Try again shortly.",
            Self::UnknownAction => "That action is not available.",
            Self::RunNotFound => "That reply is not available to you.",
            Self::TeamsUnverifiable => {
                "Neev could not verify your Teams access here, so it did nothing. Ask your Neev administrator to check the Teams connection."
            }
            Self::ChannelNotEligible => {
                "Neev only works in standard channels without guests or people from other organizations."
            }
            Self::NotATeamMember => "Neev only answers members of this Team.",
            Self::NotTeamOwner => "Only an owner of this Team can connect it to Neev.",
            Self::NotRunOwner => "Only the person this request runs as can stop it.",
        }
    }
}
