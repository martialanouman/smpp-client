//! The campaign commands of spec §15.2 (deliverable L-010-07).
//!
//! Thin, like every command module (guide §8.3): deserialise, hand the input to
//! the validating constructors of `messaging`, call the service, serialise. The
//! lifecycle, the template engine, the replay policy, the daily window and the
//! write-ahead key all live in `messaging`, so nothing here knows what a septet
//! or a `command_status` is.
//!
//! # Where the validation happens
//!
//! Not here, and that is the point of [`CampaignSendConfigInput::parse`]: every
//! field crosses through a constructor of `messaging` that *is* the validation —
//! `Template::parse`, `RetryPolicy::new`, `DailyWindow::parse`,
//! `Destination::parse_with`, `SourceAddress::parse`. What this file owns is the
//! **projection** of each rejection onto a stable `ErrorCode`, which is a
//! boundary concern.
//!
//! The WebView is untrusted (CLAUDE.md §3). A hand-made `invoke` carrying a
//! window of `99:99`, four thousand retry attempts or a template full of
//! unclosed braces takes exactly the same path as the form.
//!
//! # Why the send configuration is stored as the input that produced it
//!
//! `campaigns.send_config` is an opaque JSON document (spec §14.2), and what
//! goes in it is [`CampaignSendConfigInput`] verbatim. Two consequences, both
//! wanted:
//!
//! * a campaign resumed after a **cold restart** is rebuilt by running the
//!   stored document back through the same `parse` the create command used, so a
//!   row somebody edited by hand is validated rather than trusted (CA-010-12);
//! * there is exactly one shape to keep in step instead of a DTO and a
//!   persistence record that mean the same thing.
//!
//! # Progress is not the journal
//!
//! A campaign of 500 000 recipients does not push 500 000 events.
//! `campaign:progress` carries **aggregated counters** at a fixed cadence
//! (CA-010-11), and the per-message detail is read through `logs_query`, page by
//! page, filtered on the campaign — the same arrangement `import:progress` and
//! `message:update` already make for their own screens.

use serde::{Deserialize, Serialize};
use specta::Type;
use tauri::{AppHandle, State};

use messaging::addressing::{Destination, SourceAddress};
use messaging::campaign::runner::CampaignPlan;
use messaging::campaign::schedule::{DailyWindow, Schedule};
use messaging::retry::{RetryBackoff, RetryPolicy};
use messaging::submit::SubmitOptions;
use messaging::template::{MissingVariablePolicy, Template};
use messaging::{CampaignStatus, UnansweredPolicy};
use persistence::{Campaign, CampaignId, Cursor, ListId, ListSelection};
use smpp_core::time::Timestamp;
use smpp_core::types::SessionId;

use crate::commands::message::{
    EncodingDto, NpiDto, RegisteredDeliveryDto, SegmentationModeDto, TonDto,
};
use crate::error::ErrorDto;
use crate::state::AppState;

/// Campaigns a page holds when the interface does not say.
const DEFAULT_PAGE: u32 = 50;

/// The largest page the backend will assemble.
const MAX_PAGE: u32 = 500;

/// A statement, not a test: a default above the ceiling would be silently
/// clamped, so a caller reading `DEFAULT_PAGE` would be told one thing and
/// served another.
const _: () = {
    assert!(DEFAULT_PAGE > 0);
    assert!(DEFAULT_PAGE <= MAX_PAGE);
};

// ---------------------------------------------------------------------------
// Input DTOs
// ---------------------------------------------------------------------------

/// What to do with a recipient a template variable is missing for (CA-010-06).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase", tag = "policy")]
pub(crate) enum MissingVariableInput {
    /// Drop the recipient. The default, and the safe half of spec §10.2.
    #[default]
    Reject,
    /// Put this text in place of the placeholder.
    ///
    /// The empty string is a legitimate substitute — "greet by name when we
    /// have one" — which is why the value is not optional.
    Substitute {
        /// What replaces the placeholder.
        value: String,
    },
}

impl From<MissingVariableInput> for MissingVariablePolicy {
    fn from(input: MissingVariableInput) -> Self {
        match input {
            MissingVariableInput::Reject => Self::Reject,
            MissingVariableInput::Substitute { value } => Self::Substitute(value),
        }
    }
}

/// What to do with a message a previous run left in flight (ADR 0014).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) enum UnansweredInput {
    /// Send it again. The default: no message is lost, and the duplicate risk
    /// is counted and reported.
    #[default]
    Reemit,
    /// Leave it alone. One recipient may receive nothing, and nobody sees it.
    Abandon,
}

impl From<UnansweredInput> for UnansweredPolicy {
    fn from(input: UnansweredInput) -> Self {
        match input {
            UnansweredInput::Reemit => Self::Reemit,
            UnansweredInput::Abandon => Self::Abandon,
        }
    }
}

/// How the wait grows between two attempts (spec §10.7).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) enum RetryBackoffInput {
    /// The same wait every time.
    Fixed,
    /// Doubling, capped.
    #[default]
    Exponential,
}

impl From<RetryBackoffInput> for RetryBackoff {
    fn from(input: RetryBackoffInput) -> Self {
        match input {
            RetryBackoffInput::Fixed => Self::Fixed,
            RetryBackoffInput::Exponential => Self::Exponential,
        }
    }
}

/// The replay policy of spec §10.7.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RetryInput {
    /// Attempts per message, the first one included.
    pub(crate) max_attempts: u32,
    /// The wait before the second attempt, in seconds.
    pub(crate) initial_delay_s: u32,
    /// The ceiling the wait never passes, in seconds.
    pub(crate) max_delay_s: u32,
    /// How the wait grows.
    pub(crate) backoff: RetryBackoffInput,
}

impl Default for RetryInput {
    fn default() -> Self {
        let policy = RetryPolicy::default();

        Self {
            max_attempts: policy.max_attempts(),
            initial_delay_s: 5,
            max_delay_s: 300,
            backoff: RetryBackoffInput::Exponential,
        }
    }
}

impl RetryInput {
    /// Projects the input onto the replay policy.
    ///
    /// # Errors
    ///
    /// [`ErrorDto`] with `CAMPAIGN_INVALID_INPUT` naming `retry` when the bounds
    /// `RetryPolicy::new` enforces are broken.
    fn parse(self) -> Result<RetryPolicy, ErrorDto> {
        RetryPolicy::new(
            self.max_attempts,
            core::time::Duration::from_secs(u64::from(self.initial_delay_s)),
            core::time::Duration::from_secs(u64::from(self.max_delay_s)),
            self.backoff.into(),
        )
        .map_err(|_| ErrorDto::campaign_invalid_input("retry"))
    }
}

/// The hours of the day a campaign may send in (CA-010-10).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DailyWindowInput {
    /// When sending starts, `HH:MM`.
    pub(crate) open: String,
    /// When sending stops, `HH:MM`.
    pub(crate) close: String,
    /// Minutes east of UTC the two ends are read in — `0`, `60`, `-300`.
    ///
    /// A fixed offset and not a named zone, for the reason
    /// `messaging::campaign::schedule` states: shipping the IANA database for a
    /// "do not text people at three in the morning" setting is not a dependency
    /// CLAUDE.md §2 would accept. The consequence — an hour of drift twice a
    /// year where daylight saving applies — is shown beside the field.
    pub(crate) offset_minutes: i32,
}

/// The optional planning of a campaign (spec §10.2).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ScheduleInput {
    /// Nothing goes out before this instant, in RFC 3339. `null` for at once.
    pub(crate) start_at: Option<String>,
    /// The daily window, or `null` for every hour.
    pub(crate) window: Option<DailyWindowInput>,
}

impl ScheduleInput {
    /// Projects the input onto the planning.
    ///
    /// # Errors
    ///
    /// [`ErrorDto`] with `CAMPAIGN_INVALID_INPUT` naming `startAt` or `window`.
    fn parse(&self) -> Result<Schedule, ErrorDto> {
        let mut schedule = Schedule::immediate();

        if let Some(raw) = self.start_at.as_deref().filter(|raw| !raw.is_empty()) {
            schedule = schedule.starting_at(
                Timestamp::parse(raw).map_err(|_| ErrorDto::campaign_invalid_input("startAt"))?,
            );
        }

        if let Some(window) = self.window.as_ref() {
            schedule = schedule.within(
                DailyWindow::parse(&window.open, &window.close, window.offset_minutes)
                    .map_err(|_| ErrorDto::campaign_invalid_input("window"))?,
            );
        }

        Ok(schedule)
    }
}

/// Everything a campaign sends with, minus its name and its template.
///
/// Stored verbatim in `campaigns.send_config` — see the module header for why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CampaignSendConfigInput {
    /// Which session to send on. One session per campaign at this milestone;
    /// spreading a campaign over several is milestone 011.
    pub(crate) session_id: String,
    /// The contact list to send to, or `null` for every contact.
    pub(crate) list_id: Option<String>,
    /// Lists whose members are excluded whatever else says.
    #[serde(default)]
    pub(crate) excluded_list_ids: Vec<String>,
    /// The sender, or `null` to let the message centre choose one.
    pub(crate) source: Option<String>,
    /// `dest_addr_ton` of every recipient.
    pub(crate) dest_ton: TonDto,
    /// `dest_addr_npi` of every recipient.
    pub(crate) dest_npi: NpiDto,
    /// Which alphabet to write the messages in.
    pub(crate) encoding: EncodingDto,
    /// How a long message announces its parts.
    pub(crate) segmentation_mode: SegmentationModeDto,
    /// What to ask for in `registered_delivery`.
    pub(crate) registered_delivery: RegisteredDeliveryDto,
    /// What to do with a recipient a variable is missing for.
    #[serde(default)]
    pub(crate) on_missing_variable: MissingVariableInput,
    /// What to do with a message a previous run left in flight.
    #[serde(default)]
    pub(crate) on_unanswered: UnansweredInput,
    /// The replay policy.
    pub(crate) retry: RetryInput,
    /// The optional planning.
    #[serde(default)]
    pub(crate) schedule: ScheduleInput,
}

/// A validated configuration, ready to run.
#[derive(Debug)]
pub(crate) struct CampaignSetup {
    /// The session the campaign sends on.
    pub(crate) session_id: SessionId,
    /// Which contacts it sends to.
    pub(crate) selection: ListSelection,
    /// Everything else, as the runner takes it.
    pub(crate) plan: CampaignPlan,
}

impl CampaignSendConfigInput {
    /// Rebuilds the domain plan, validating every field.
    ///
    /// # Errors
    ///
    /// [`ErrorDto`] with `CAMPAIGN_INVALID_INPUT` naming the offending field, or
    /// a `MESSAGE_*` code when an address is refused — the same codes
    /// `message_send` reports, because it is the same validation.
    pub(crate) fn parse(
        &self,
        campaign_id: CampaignId,
        template: &str,
    ) -> Result<CampaignSetup, ErrorDto> {
        let session_id = SessionId::parse(&self.session_id)
            .map_err(|_| ErrorDto::campaign_invalid_input("sessionId"))?;

        let template =
            Template::parse(template).map_err(|_| ErrorDto::campaign_invalid_input("template"))?;

        // A placeholder recipient: every field of `SubmitOptions` is used as
        // written except the destination, which the runner replaces per
        // recipient. `CampaignPlan` says so at length. It still has to *parse*,
        // and parsing it under the campaign's own TON and NPI is what makes a
        // combination the message centre would refuse fail at creation rather
        // than on the first of two hundred thousand messages.
        let placeholder = Destination::parse_with(
            PLACEHOLDER_DESTINATION,
            self.dest_ton.into(),
            self.dest_npi.into(),
        )
        .map_err(|error| ErrorDto::from(&error))?;

        let mut submit = SubmitOptions::to(placeholder);

        if let Some(raw) = self.source.as_deref().filter(|raw| !raw.trim().is_empty()) {
            submit = submit
                .with_source(SourceAddress::parse(raw).map_err(|error| ErrorDto::from(&error))?);
        }

        submit.registered_delivery = self.registered_delivery.into();

        let plan = CampaignPlan::new(campaign_id, template, submit)
            .on_missing_variable(self.on_missing_variable.clone().into())
            .on_unanswered(self.on_unanswered.into())
            .with_retry(self.retry.parse()?)
            .scheduled(self.schedule.parse()?)
            .with_encoding(self.encoding.into())
            .with_mode(self.segmentation_mode.into())
            .addressed_as(self.dest_ton.into(), self.dest_npi.into());

        Ok(CampaignSetup {
            session_id,
            selection: self.selection()?,
            plan,
        })
    }

    /// Which contacts the campaign sends to.
    fn selection(&self) -> Result<ListSelection, ErrorDto> {
        let selection = match self.list_id.as_deref().filter(|raw| !raw.is_empty()) {
            None => ListSelection::everything(),
            Some(raw) => ListSelection::union(vec![
                ListId::parse(raw).ok_or_else(|| ErrorDto::campaign_invalid_input("listId"))?
            ]),
        };

        let excluded = self
            .excluded_list_ids
            .iter()
            .filter(|raw| !raw.is_empty())
            .map(|raw| {
                ListId::parse(raw)
                    .ok_or_else(|| ErrorDto::campaign_invalid_input("excludedListIds"))
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok(selection.excluding(excluded))
    }
}

/// The number the placeholder destination is parsed from.
///
/// Never sent to: [`CampaignPlan`] replaces the destination with the recipient
/// the feeder resolved, for every message. It exists because `SubmitOptions`
/// carries a recipient and a campaign has one per message — the alternative
/// being a second options type with one field removed.
const PLACEHOLDER_DESTINATION: &str = "+10000000000";

/// Input of [`campaign_create`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CampaignCreateInput {
    /// The name shown in the interface.
    pub(crate) name: String,
    /// The message template, with its `{{variables}}` (spec §10.2).
    pub(crate) template: String,
    /// Everything the campaign sends with.
    pub(crate) config: CampaignSendConfigInput,
}

// ---------------------------------------------------------------------------
// Output DTOs
// ---------------------------------------------------------------------------

/// One campaign, as the interface lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CampaignRowDto {
    /// Primary key, and the row's React key.
    pub(crate) campaign_id: String,
    /// The name the operator gave it.
    pub(crate) name: String,
    /// `CREATED`, `VALIDATED`, `RUNNING`… — the names of spec §10.3.
    pub(crate) status: String,
    /// The message template.
    pub(crate) template: String,
    /// Everything it sends with, or `null` when the stored document no longer
    /// parses.
    ///
    /// **Nullable on purpose.** A configuration written by an older version, or
    /// edited by hand, must not make the whole list unreadable: the campaign is
    /// shown, and the interface refuses to start it rather than pretending it
    /// has settings it cannot read.
    pub(crate) config: Option<CampaignSendConfigInput>,
    /// Recipients the campaign was created over.
    pub(crate) total: u32,
    /// Messages the message centre accepted, as of the last write.
    pub(crate) sent: u32,
    /// Messages a delivery receipt confirmed.
    pub(crate) delivered: u32,
    /// Messages that failed for good.
    pub(crate) failed: u32,
    /// Whether a run of this campaign is live in this process **right now**.
    ///
    /// Not derivable from [`Self::status`], and that is the whole reason it is
    /// sent: a process killed mid-campaign leaves a row reading `RUNNING` with
    /// nothing behind it, and that is exactly the campaign the interface must
    /// offer to resume rather than to pause.
    pub(crate) live: bool,
    /// When it was created.
    pub(crate) created_at: String,
    /// When sending began.
    pub(crate) started_at: Option<String>,
    /// When it reached a terminal status.
    pub(crate) completed_at: Option<String>,
}

impl CampaignRowDto {
    /// Projects one stored campaign.
    fn of(campaign: Campaign, live: bool) -> Self {
        Self {
            campaign_id: campaign.campaign_id.to_string(),
            name: campaign.name,
            status: campaign.status.as_str().to_owned(),
            template: campaign.template,
            config: serde_json::from_str(&campaign.send_config).ok(),
            total: campaign.total_count,
            sent: campaign.sent_count,
            delivered: campaign.delivered_count,
            failed: campaign.failed_count,
            live,
            created_at: campaign.created_at.to_storage(),
            started_at: campaign.started_at.map(|instant| instant.to_storage()),
            completed_at: campaign.completed_at.map(|instant| instant.to_storage()),
        }
    }
}

/// One page of the campaign list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CampaignPageDto {
    /// The campaigns, oldest first.
    pub(crate) rows: Vec<CampaignRowDto>,
    /// Cursor to pass back for the next page, or `null` at the end.
    pub(crate) next: Option<String>,
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// Creates a campaign (EF-MSG-02, spec §10.2).
///
/// Validates the template and the whole configuration, counts the recipients the
/// selection picks out, and stores the campaign as `VALIDATED`.
///
/// # Why it is stored `VALIDATED` and not `CREATED`
///
/// Spec §10.3 gates sending behind a validation step, and this command **is**
/// it: the template has parsed, every field has crossed a constructor of
/// `messaging`, and the recipients have been counted. A row stored `CREATED`
/// would need a second command to move it, which spec §15.2 does not list and
/// which would have nothing left to check. The machine still refuses
/// `CREATED → RUNNING`, so a row hand-edited back to `CREATED` cannot be
/// started.
///
/// # Errors
///
/// * `CAMPAIGN_INVALID_INPUT` — a field could not be read; `details` names it;
/// * `MESSAGE_INVALID_SOURCE` — the sender address was refused;
/// * `CAMPAIGN_NO_RECIPIENTS` — the selection picks nobody out;
/// * `CAMPAIGN_STORAGE` — the store refused the write.
#[tauri::command]
#[specta::specta]
pub(crate) async fn campaign_create(
    state: State<'_, AppState>,
    input: CampaignCreateInput,
) -> Result<String, ErrorDto> {
    let name = input.name.trim();

    // Validated here and not in the form: the WebView is untrusted, and a
    // campaign with no name is one an operator can never pick out of a list
    // again.
    if name.is_empty() {
        return Err(ErrorDto::campaign_invalid_input("name"));
    }

    let campaign_id = CampaignId::new();
    let setup = input.config.parse(campaign_id, &input.template)?;

    let total = state.campaigns().count_recipients(&setup.selection).await?;

    if total == 0 {
        return Err(ErrorDto::campaign_no_recipients());
    }

    let campaign = Campaign {
        campaign_id,
        name: name.to_owned(),
        status: CampaignStatus::Validated,
        template: input.template.clone(),
        send_config: serde_json::to_string(&input.config)
            .map_err(|_| ErrorDto::campaign_invalid_input("config"))?,
        total_count: u32::try_from(total).unwrap_or(u32::MAX),
        sent_count: 0,
        delivered_count: 0,
        failed_count: 0,
        created_at: Timestamp::now(),
        started_at: None,
        completed_at: None,
    };

    state.campaigns().save(&campaign).await?;

    Ok(campaign_id.to_string())
}

/// One page of campaigns, oldest first.
///
/// # Errors
///
/// [`ErrorDto`] with `CAMPAIGN_INVALID_INPUT` for a malformed cursor, or
/// `CAMPAIGN_STORAGE` if the store will not answer.
#[tauri::command]
#[specta::specta]
pub(crate) async fn campaign_list(
    state: State<'_, AppState>,
    cursor: Option<String>,
    limit: Option<u32>,
) -> Result<CampaignPageDto, ErrorDto> {
    let cursor = match cursor.as_deref().filter(|raw| !raw.is_empty()) {
        None => Cursor::start(),
        Some(raw) => raw
            .parse::<i64>()
            .map(Cursor::from_raw)
            .map_err(|_| ErrorDto::campaign_invalid_input("cursor"))?,
    };

    let page = state
        .campaigns()
        .page(cursor, limit.unwrap_or(DEFAULT_PAGE).clamp(1, MAX_PAGE))
        .await?;

    let mut rows = Vec::with_capacity(page.items.len());

    for campaign in page.items {
        let live = state.campaigns().is_running(campaign.campaign_id).await;

        rows.push(CampaignRowDto::of(campaign, live));
    }

    Ok(CampaignPageDto {
        rows,
        next: page.next.map(|position| position.into_raw().to_string()),
    })
}

/// Starts a campaign (spec §10.3).
///
/// Returns as soon as the campaign is running, **not** when it has finished: a
/// campaign of half a million recipients runs for hours. Progress arrives on
/// `campaign:progress`, and the per-message detail through `logs_query`.
///
/// # Errors
///
/// * `CAMPAIGN_INVALID_INPUT` — the identifier or the stored configuration
///   could not be read;
/// * `CAMPAIGN_NOT_FOUND` — no campaign carries that identifier;
/// * `CAMPAIGN_BUSY` — it is already running;
/// * `CAMPAIGN_INVALID_TRANSITION` — the lifecycle refuses it; `details` carries
///   `from` and `to`;
/// * `CAMPAIGN_SESSION_NOT_BOUND` — its session is not open;
/// * `CAMPAIGN_STORAGE` — the status could not be written.
#[tauri::command]
#[specta::specta]
pub(crate) async fn campaign_start(
    app: AppHandle,
    state: State<'_, AppState>,
    campaign_id: String,
) -> Result<(), ErrorDto> {
    launch(&app, &state, &campaign_id, false).await
}

/// Suspends the feeding of a running campaign (spec §10.3, CA-010-03).
///
/// The messages already in the send window finish normally and the session stays
/// bound; only the feeding stops.
///
/// # Errors
///
/// `CAMPAIGN_NOT_FOUND`, `CAMPAIGN_INVALID_TRANSITION` or `CAMPAIGN_STORAGE`.
#[tauri::command]
#[specta::specta]
pub(crate) async fn campaign_pause(
    state: State<'_, AppState>,
    campaign_id: String,
) -> Result<(), ErrorDto> {
    state.campaigns().pause(parse_id(&campaign_id)?).await
}

/// Resumes a campaign (spec §10.3, CA-010-03).
///
/// Two different things behind one command, and the interface does not have to
/// know which:
///
/// * a campaign **paused in this process** is told to carry on, and picks up
///   from the queue it was holding;
/// * a campaign a restart or a crash left behind is **run again in resuming
///   mode** — the runner then asks the journal about every recipient before
///   emitting, so a message already accepted is never sent twice (CA-010-05).
///
/// # Errors
///
/// The codes of [`campaign_start`], plus `CAMPAIGN_NOT_FOUND`.
#[tauri::command]
#[specta::specta]
pub(crate) async fn campaign_resume(
    app: AppHandle,
    state: State<'_, AppState>,
    campaign_id: String,
) -> Result<(), ErrorDto> {
    if state
        .campaigns()
        .resume_in_place(parse_id(&campaign_id)?)
        .await?
    {
        return Ok(());
    }

    launch(&app, &state, &campaign_id, true).await
}

/// Stops a campaign for good (CA-010-09).
///
/// The emission stops at once; the messages already on the wire are journalled
/// rather than abandoned, and every recipient the campaign queued ends in one of
/// its counters.
///
/// # Errors
///
/// `CAMPAIGN_NOT_FOUND`, `CAMPAIGN_INVALID_TRANSITION` or `CAMPAIGN_STORAGE`.
#[tauri::command]
#[specta::specta]
pub(crate) async fn campaign_cancel(
    state: State<'_, AppState>,
    campaign_id: String,
) -> Result<(), ErrorDto> {
    state.campaigns().cancel(parse_id(&campaign_id)?).await
}

/// Reads a campaign identifier.
fn parse_id(raw: &str) -> Result<CampaignId, ErrorDto> {
    CampaignId::parse(raw).map_err(|_| ErrorDto::campaign_invalid_input("campaignId"))
}

/// Starts one run of a campaign, fresh or resuming.
///
/// Shared by [`campaign_start`] and [`campaign_resume`] because the two differ
/// in exactly one bit: whether the runner asks the journal about every recipient
/// before emitting. Writing it twice is how the two would drift.
async fn launch(
    app: &AppHandle,
    state: &State<'_, AppState>,
    campaign_id: &str,
    resuming: bool,
) -> Result<(), ErrorDto> {
    let campaign = state.campaigns().find(parse_id(campaign_id)?).await?;

    let config: CampaignSendConfigInput = serde_json::from_str(&campaign.send_config)
        .map_err(|_| ErrorDto::campaign_invalid_input("config"))?;

    // Re-validated on every start, and not only at creation: the stored
    // document is a row of a SQLite file the operator owns, and CLAUDE.md §3
    // treats everything that crosses into the backend as untrusted. It costs
    // one parse per campaign start.
    let setup = config.parse(campaign.campaign_id, &campaign.template)?;

    let handle = state
        .sessions()
        .registry()
        .handle(setup.session_id)
        .await
        .ok_or_else(ErrorDto::campaign_session_not_bound)?;

    let plan = if resuming {
        setup.plan.resuming()
    } else {
        setup.plan
    };

    state
        .campaigns()
        .start(app, &campaign, handle, plan, setup.selection)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a_config() -> CampaignSendConfigInput {
        CampaignSendConfigInput {
            session_id: SessionId::new().to_string(),
            list_id: None,
            excluded_list_ids: Vec::new(),
            source: Some("ShinobiSMS".to_owned()),
            dest_ton: TonDto::International,
            dest_npi: NpiDto::Isdn,
            encoding: EncodingDto::Automatic,
            segmentation_mode: SegmentationModeDto::Udh,
            registered_delivery: RegisteredDeliveryDto::OnAnyOutcome,
            on_missing_variable: MissingVariableInput::Reject,
            on_unanswered: UnansweredInput::Reemit,
            retry: RetryInput::default(),
            schedule: ScheduleInput::default(),
        }
    }

    fn a_campaign() -> CampaignId {
        CampaignId::parse("3f8d0a2e-0000-4000-8000-000000000001").expect("a valid UUID")
    }

    #[test]
    fn a_well_formed_configuration_parses_into_a_plan() {
        let setup = a_config()
            .parse(a_campaign(), "Bonjour {{prenom}}")
            .expect("the fixture is valid");

        assert_eq!(setup.plan.campaign_id, a_campaign());
        assert_eq!(
            setup.plan.retry.max_attempts(),
            RetryPolicy::default().max_attempts()
        );
        assert!(setup.plan.submit.source.is_some());
    }

    /// CA-010-06 begins here: a template the engine cannot read is refused
    /// before a campaign exists, not on the first of two hundred thousand
    /// messages.
    #[test]
    fn a_template_that_does_not_parse_is_refused_at_the_boundary() {
        let rejection = a_config()
            .parse(a_campaign(), "Bonjour {{prenom")
            .expect_err("an unclosed placeholder");

        assert_eq!(
            rejection.code,
            crate::error::ErrorCode::CampaignInvalidInput
        );
        assert_eq!(
            rejection
                .details
                .as_ref()
                .and_then(|details| details.get("field"))
                .map(String::as_str),
            Some("template")
        );
    }

    /// The WebView is untrusted: bounds the form would have enforced are
    /// enforced here too.
    #[test]
    fn a_replay_policy_outside_its_bounds_is_refused() {
        for retry in [
            RetryInput {
                max_attempts: 0,
                ..RetryInput::default()
            },
            RetryInput {
                max_attempts: 100_000,
                ..RetryInput::default()
            },
            RetryInput {
                initial_delay_s: 0,
                ..RetryInput::default()
            },
            RetryInput {
                initial_delay_s: 600,
                max_delay_s: 60,
                ..RetryInput::default()
            },
        ] {
            let config = CampaignSendConfigInput {
                retry,
                ..a_config()
            };

            assert_eq!(
                config
                    .parse(a_campaign(), "Bonjour")
                    .expect_err("out of bounds")
                    .code,
                crate::error::ErrorCode::CampaignInvalidInput,
                "{retry:?}"
            );
        }
    }

    #[test]
    fn a_daily_window_that_does_not_parse_is_refused_by_name() {
        let config = CampaignSendConfigInput {
            schedule: ScheduleInput {
                start_at: None,
                window: Some(DailyWindowInput {
                    open: "8h00".to_owned(),
                    close: "20:00".to_owned(),
                    offset_minutes: 0,
                }),
            },
            ..a_config()
        };

        let rejection = config
            .parse(a_campaign(), "Bonjour")
            .expect_err("not an HH:MM time");

        assert_eq!(
            rejection
                .details
                .as_ref()
                .and_then(|details| details.get("field"))
                .map(String::as_str),
            Some("window")
        );
    }

    #[test]
    fn a_deferred_start_reaches_the_plan() {
        let config = CampaignSendConfigInput {
            schedule: ScheduleInput {
                start_at: Some("2026-08-02T20:00:00Z".to_owned()),
                window: None,
            },
            ..a_config()
        };

        let setup = config
            .parse(a_campaign(), "Bonjour")
            .expect("a valid instant");

        assert_eq!(
            setup.plan.schedule.start_at(),
            Some(Timestamp::parse("2026-08-02T20:00:00Z").expect("valid"))
        );
    }

    #[test]
    fn a_start_instant_that_is_not_one_is_refused_by_name() {
        let config = CampaignSendConfigInput {
            schedule: ScheduleInput {
                start_at: Some("demain matin".to_owned()),
                window: None,
            },
            ..a_config()
        };

        assert_eq!(
            config
                .parse(a_campaign(), "Bonjour")
                .expect_err("not an instant")
                .details
                .as_ref()
                .and_then(|details| details.get("field"))
                .map(String::as_str),
            Some("startAt")
        );
    }

    #[test]
    fn a_session_identifier_that_is_not_a_uuid_is_refused_by_name() {
        let config = CampaignSendConfigInput {
            session_id: "not-a-uuid".to_owned(),
            ..a_config()
        };

        assert_eq!(
            config
                .parse(a_campaign(), "Bonjour")
                .expect_err("not a UUID")
                .details
                .as_ref()
                .and_then(|details| details.get("field"))
                .map(String::as_str),
            Some("sessionId")
        );
    }

    /// An oversized sender ID is refused with the **message** code, because it
    /// is the same rejection `message_send` reports for the same field. A
    /// campaign-specific code for it would be a second thing to translate that
    /// means the same.
    #[test]
    fn an_oversized_sender_id_is_refused_whatever_the_form_allowed() {
        let config = CampaignSendConfigInput {
            source: Some("A".repeat(12)),
            ..a_config()
        };

        assert_eq!(
            config
                .parse(a_campaign(), "Bonjour")
                .expect_err("twelve characters")
                .code,
            crate::error::ErrorCode::MessageInvalidSource
        );
    }

    /// The one property the stored document has to have: what `parse` accepted
    /// at creation is what a cold restart reads back (CA-010-12). A field added
    /// to the input without a `#[serde(default)]` or a migration fails here.
    #[test]
    fn a_stored_configuration_round_trips_through_its_json_form() {
        let config = CampaignSendConfigInput {
            list_id: Some(ListId::new().to_string()),
            excluded_list_ids: vec![ListId::new().to_string()],
            on_missing_variable: MissingVariableInput::Substitute {
                value: "cher client".to_owned(),
            },
            on_unanswered: UnansweredInput::Abandon,
            schedule: ScheduleInput {
                start_at: Some("2026-08-02T20:00:00Z".to_owned()),
                window: Some(DailyWindowInput {
                    open: "08:00".to_owned(),
                    close: "20:00".to_owned(),
                    offset_minutes: -300,
                }),
            },
            ..a_config()
        };

        let stored = serde_json::to_string(&config).expect("the document serialises");
        let read: CampaignSendConfigInput = serde_json::from_str(&stored).expect("and reads back");

        assert_eq!(read, config);
        assert!(read.parse(a_campaign(), "Bonjour {{prenom}}").is_ok());
    }

    /// A list identifier that is not one is refused rather than quietly widened
    /// to "every contact" — which would send the campaign to the whole store.
    #[test]
    fn a_malformed_list_identifier_is_refused_rather_than_ignored() {
        let config = CampaignSendConfigInput {
            list_id: Some("not-a-uuid".to_owned()),
            ..a_config()
        };

        assert_eq!(
            config
                .parse(a_campaign(), "Bonjour")
                .expect_err("not a UUID")
                .details
                .as_ref()
                .and_then(|details| details.get("field"))
                .map(String::as_str),
            Some("listId")
        );
    }

    #[test]
    fn no_list_means_every_contact_and_a_list_means_that_list() {
        assert_eq!(
            a_config().selection().expect("valid"),
            ListSelection::everything()
        );

        let list = ListId::new();
        let config = CampaignSendConfigInput {
            list_id: Some(list.to_string()),
            ..a_config()
        };

        assert_eq!(
            config.selection().expect("valid"),
            ListSelection::union(vec![list])
        );
    }
}
