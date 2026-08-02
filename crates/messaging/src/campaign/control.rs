//! Start, pause, resume, cancel — the four controls of spec §10.3.
//!
//! One [`CampaignControl`] per campaign, held by whoever runs it; as many
//! [`ControlHandle`]s as there are tasks in it. The feeder and the emitter both
//! ask the same question — *may I do the next thing?* — and get the same answer.
//!
//! # Why two primitives rather than one
//!
//! | Primitive | Carries | Why it cannot be the other one |
//! |---|---|---|
//! | [`tokio::sync::watch`] | `Running` / `Paused` / `Cancelled` | a token has one edge, and pause is a state that comes back |
//! | [`CancellationToken`] | stop, for ever | it is what the rest of the workspace watches, and it composes with the application's own (CLAUDE.md §4) |
//!
//! The token is not a duplicate of the `Cancelled` state: it is what lets a
//! campaign be a **child** of the application's shutdown token, so quitting
//! stops it without anybody routing a command, and what lets a `select!` in the
//! middle of a retry delay watch one thing rather than a channel and a flag.
//! The two are set together and never diverge — [`CampaignControl::cancel`] is
//! the only writer of either.
//!
//! # Cancellation is terminal, pause is not
//!
//! A `resume` after a `cancel` is a no-op, and so is a `pause`. This is the same
//! rule [`super::CampaignStatus`] states for the lifecycle — nothing leaves a
//! terminal status — enforced here as well because the two are reached by
//! different paths: a cancellation may arrive from the parent token, which never
//! goes through the status machine at all.

use tokio::sync::watch;
use tokio_util::sync::{CancellationToken, WaitForCancellationFuture};

/// What a campaign has been told to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum RunState {
    /// Feed and emit.
    Running,
    /// Stop feeding; the messages already in the window finish normally
    /// (spec §10.3).
    Paused,
    /// Stop, for good.
    Cancelled,
}

/// What a task waiting to proceed was told.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Resumption {
    /// Carry on.
    Proceed,
    /// Stop: the campaign was cancelled, or nobody is driving it any more.
    Cancelled,
}

/// The four controls of one campaign.
///
/// Dropping it cancels the campaign. That is deliberate rather than incidental:
/// the tasks holding a [`ControlHandle`] wait for commands, and once the owner
/// is gone no command can arrive — a task left waiting on it would be exactly
/// the orphan CLAUDE.md §4 forbids.
#[derive(Debug)]
pub struct CampaignControl {
    state: watch::Sender<RunState>,
    cancel: CancellationToken,
}

impl CampaignControl {
    /// A control over a campaign that is running, watched by its own token.
    #[must_use]
    pub fn new() -> Self {
        Self::with_token(CancellationToken::new())
    }

    /// A control whose campaign stops when `parent` is cancelled.
    ///
    /// How a campaign joins the application's shutdown tree: quitting cancels
    /// the parent, and every campaign under it stops without a command being
    /// routed anywhere.
    #[must_use]
    pub fn under(parent: &CancellationToken) -> Self {
        Self::with_token(parent.child_token())
    }

    /// A control over a campaign watched by `cancel`.
    #[must_use]
    pub fn with_token(cancel: CancellationToken) -> Self {
        let (state, _) = watch::channel(RunState::Running);

        Self { state, cancel }
    }

    /// A handle for one task of this campaign.
    #[must_use]
    pub fn handle(&self) -> ControlHandle {
        ControlHandle {
            state: self.state.subscribe(),
            cancel: self.cancel.clone(),
        }
    }

    /// The token this campaign watches, for a `select!` that must be
    /// interrupted — a retry delay, a scheduled start (CA-010-09).
    #[must_use]
    pub const fn token(&self) -> &CancellationToken {
        &self.cancel
    }

    /// What the campaign has been told to do.
    #[must_use]
    pub fn state(&self) -> RunState {
        if self.cancel.is_cancelled() {
            return RunState::Cancelled;
        }

        *self.state.borrow()
    }

    /// Suspends the feeding. A no-op on a cancelled campaign.
    pub fn pause(&self) {
        self.set(RunState::Paused);
    }

    /// Resumes the feeding. A no-op on a cancelled campaign.
    ///
    /// "No-op" is enforced by the token and not by a second check here — see
    /// [`Self::set`].
    pub fn resume(&self) {
        self.set(RunState::Running);
    }

    /// Stops the campaign, for good.
    ///
    /// Sets the token **and** the state, in that order, so a task that reads the
    /// state after seeing the token cancelled cannot find `Running`.
    pub fn cancel(&self) {
        self.cancel.cancel();
        self.state.send_replace(RunState::Cancelled);
    }

    /// Moves to `next`.
    ///
    /// # There is deliberately no "unless it is cancelled" check here
    ///
    /// It was written, and it was **unobservable**: every reader — [`Self::state`]
    /// and [`ControlHandle::wait_until_running`] — consults the token *before*
    /// the watch, because a cancellation may arrive from the parent token
    /// without passing through this type at all. So the watch holding `Running`
    /// after a cancellation changes nothing anybody can see, and a second guard
    /// that no test can distinguish is a guard nobody maintains: deleting it
    /// left every test in this module green, which is how it was found.
    ///
    /// Terminality is therefore enforced in **one** place, the token, and the
    /// tests above hold both readers to it.
    fn set(&self, next: RunState) {
        // The result is deliberately ignored: `send` fails only when no handle
        // is subscribed, which is a campaign whose tasks have finished. Pausing
        // one is a no-op, not an error.
        self.state.send_replace(next);
    }
}

impl Default for CampaignControl {
    fn default() -> Self {
        Self::new()
    }
}

impl From<RunState> for super::CampaignStatus {
    /// The lifecycle status a campaign under this command is **in**.
    ///
    /// # Why this projection exists at all
    ///
    /// Whoever reports on a running campaign — the progress sampler of the IPC
    /// layer — holds the control and not the row. Asking the database four
    /// times a second for a status it already knows would be a read per
    /// reading; hard-wiring `Running` instead is what a first cut did, and it
    /// made a paused campaign publish `RUNNING` 250 ms after the operator
    /// paused it, taking the resume button away with it.
    ///
    /// # `Cancelled` is reported as soon as it is asked for
    ///
    /// A cancelled campaign is not finished: it drains its queue and journals
    /// what is in flight before its task returns. Reporting `Running` for that
    /// stretch would offer a pause on a campaign that is stopping, and
    /// inventing a "stopping" status would add an eighth to a machine spec
    /// §10.3 closes at seven. `CANCELLED` is where it is going, the operator
    /// asked for it, and the reading that carries it also says the counters are
    /// still moving.
    fn from(state: RunState) -> Self {
        match state {
            RunState::Running => Self::Running,
            RunState::Paused => Self::Paused,
            RunState::Cancelled => Self::Cancelled,
        }
    }
}

/// One task's view of the controls.
///
/// Cloneable, and each clone tracks its own "has it changed since I looked"
/// mark — which is why [`Self::wait_until_running`] takes `&mut self`.
#[derive(Debug, Clone)]
pub struct ControlHandle {
    state: watch::Receiver<RunState>,
    cancel: CancellationToken,
}

impl ControlHandle {
    /// Waits until the campaign is running, or until it is cancelled.
    ///
    /// Returns as soon as it is called on a running campaign — the nominal
    /// path, once per recipient — and parks otherwise. The park is woken by a
    /// resume, by a cancellation, by the parent token, or by the control being
    /// dropped; there is no fifth outcome and no timeout, so nothing here can
    /// leave a task waiting on a state that will not come.
    pub async fn wait_until_running(&mut self) -> Resumption {
        loop {
            if self.cancel.is_cancelled() {
                return Resumption::Cancelled;
            }

            match *self.state.borrow_and_update() {
                RunState::Running => return Resumption::Proceed,
                RunState::Cancelled => return Resumption::Cancelled,
                RunState::Paused => {}
            }

            tokio::select! {
                biased;

                () = self.cancel.cancelled() => return Resumption::Cancelled,
                // `Err` means the control was dropped: no further command can
                // arrive, so waiting on one is waiting for ever.
                changed = self.state.changed() => {
                    if changed.is_err() {
                        return Resumption::Cancelled;
                    }
                }
            }
        }
    }

    /// Whether the campaign has been cancelled.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    /// Resolves when the campaign is cancelled.
    ///
    /// What a `select!` watches while something else is in flight: a retry
    /// delay, a scheduled start, a push into a full queue.
    pub fn cancelled(&self) -> WaitForCancellationFuture<'_> {
        self.cancel.cancelled()
    }

    /// The token itself, for a caller that has to hand one down.
    #[must_use]
    pub const fn token(&self) -> &CancellationToken {
        &self.cancel
    }
}

#[cfg(test)]
mod tests {
    // `#[tokio::test]` expands to `Runtime::block_on`, which `clippy.toml`
    // reserves for "the binary entry point". A test harness is one.
    #![allow(clippy::disallowed_methods)]

    use super::{CampaignControl, Resumption, RunState};
    use core::time::Duration;
    use tokio_util::sync::CancellationToken;

    /// How long a test waits before concluding that a future is not going to
    /// resolve. Virtual time, under `start_paused`: it costs nothing and it is
    /// the same on every machine.
    const SETTLE: Duration = Duration::from_millis(50);

    #[tokio::test]
    async fn a_running_campaign_proceeds_without_waiting() {
        let control = CampaignControl::new();
        let mut handle = control.handle();

        assert_eq!(control.state(), RunState::Running);
        assert_eq!(handle.wait_until_running().await, Resumption::Proceed);
    }

    #[tokio::test(start_paused = true)]
    async fn a_paused_campaign_waits_and_then_proceeds_when_resumed() {
        let control = CampaignControl::new();
        let mut handle = control.handle();

        control.pause();
        assert_eq!(control.state(), RunState::Paused);

        assert!(
            tokio::time::timeout(SETTLE, handle.wait_until_running())
                .await
                .is_err(),
            "a paused campaign must not feed"
        );

        control.resume();

        assert_eq!(
            tokio::time::timeout(SETTLE, handle.wait_until_running())
                .await
                .expect("a resumed campaign proceeds"),
            Resumption::Proceed
        );
    }

    /// CA-010-09: cancelling has to reach a task that is *parked*, which is the
    /// state a paused campaign is in. A cancellation that only the running loop
    /// noticed would leave a paused campaign waiting for ever.
    #[tokio::test(start_paused = true)]
    async fn cancelling_wakes_a_paused_campaign() {
        let control = CampaignControl::new();
        let mut handle = control.handle();

        control.pause();
        control.cancel();

        assert_eq!(
            tokio::time::timeout(SETTLE, handle.wait_until_running())
                .await
                .expect("cancellation wakes the waiter"),
            Resumption::Cancelled
        );
    }

    #[tokio::test]
    async fn a_cancelled_campaign_never_runs_again() {
        let control = CampaignControl::new();
        let mut handle = control.handle();

        control.cancel();
        control.resume();

        assert_eq!(control.state(), RunState::Cancelled);
        assert_eq!(handle.wait_until_running().await, Resumption::Cancelled);
    }

    #[tokio::test]
    async fn a_cancelled_campaign_cannot_be_paused_back_into_life() {
        let control = CampaignControl::new();

        control.cancel();
        control.pause();

        assert_eq!(control.state(), RunState::Cancelled);
    }

    /// The application shutting down is not a campaign command, and it still
    /// has to stop the campaign — CLAUDE.md §4 asks every long task to watch one
    /// token, and this is how a campaign joins the tree.
    #[tokio::test(start_paused = true)]
    async fn cancelling_the_parent_token_cancels_the_campaign() {
        let parent = CancellationToken::new();
        let control = CampaignControl::under(&parent);
        let mut handle = control.handle();

        control.pause();
        parent.cancel();

        assert_eq!(
            tokio::time::timeout(SETTLE, handle.wait_until_running())
                .await
                .expect("the parent token wakes the waiter"),
            Resumption::Cancelled
        );
        assert!(handle.is_cancelled());
        // The state has to follow the token, although nothing wrote it: a
        // screen reading `PAUSED` for a campaign the shutdown stopped would
        // offer a resume button that does nothing.
        assert_eq!(control.state(), RunState::Cancelled);
    }

    /// The case the test above does **not** cover, and the one that happens: the
    /// task is already **parked** when the parent token fires.
    ///
    /// Written after a mutation showed the difference. Deleting the token arm of
    /// the `select!` left every other test in this module green, because a
    /// cancellation issued through [`CampaignControl::cancel`] also writes the
    /// state and wakes the watch. The parent token writes nothing, so a waiter
    /// already parked is woken by that arm and by nothing else — an application
    /// quitting while a campaign is paused would hang for ever without it.
    #[tokio::test(start_paused = true)]
    async fn a_parked_task_is_woken_by_the_parent_token() {
        let parent = CancellationToken::new();
        let control = CampaignControl::under(&parent);
        let mut handle = control.handle();

        control.pause();

        let (resumption, ()) = tokio::join!(handle.wait_until_running(), async {
            // Under `start_paused` the clock only advances once every task is
            // idle, so this sleep returning proves the waiter is parked.
            tokio::time::sleep(SETTLE).await;
            parent.cancel();
        });

        assert_eq!(resumption, Resumption::Cancelled);
    }

    /// The same, for a cancellation issued as a command rather than inherited.
    #[tokio::test(start_paused = true)]
    async fn a_parked_task_is_woken_by_a_cancellation() {
        let control = CampaignControl::new();
        let mut handle = control.handle();

        control.pause();

        let (resumption, ()) = tokio::join!(handle.wait_until_running(), async {
            tokio::time::sleep(SETTLE).await;
            control.cancel();
        });

        assert_eq!(resumption, Resumption::Cancelled);
    }

    /// A campaign is cancelled, not left hanging, when whoever was driving it
    /// disappears: the handle is held by tasks that would otherwise wait on a
    /// command nobody can send any more.
    #[tokio::test(start_paused = true)]
    async fn dropping_the_control_cancels_the_campaign() {
        let control = CampaignControl::new();
        let mut handle = control.handle();

        control.pause();
        drop(control);

        assert_eq!(
            tokio::time::timeout(SETTLE, handle.wait_until_running())
                .await
                .expect("a dropped control wakes the waiter"),
            Resumption::Cancelled
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_cancellation_future_resolves_on_cancellation() {
        let control = CampaignControl::new();
        let handle = control.handle();

        assert!(!handle.is_cancelled());
        assert!(tokio::time::timeout(SETTLE, handle.cancelled())
            .await
            .is_err());

        control.cancel();

        assert!(tokio::time::timeout(SETTLE, handle.cancelled())
            .await
            .is_ok());
        assert!(handle.is_cancelled());
    }

    /// Every task of one campaign watches the same commands: the feeder stops
    /// reading, the emitter stops emitting, and neither can be resumed while the
    /// other is not.
    #[tokio::test(start_paused = true)]
    async fn every_handle_of_one_campaign_sees_the_same_commands() {
        let control = CampaignControl::new();
        let mut feeder = control.handle();
        let mut emitter = control.handle();

        control.pause();

        assert!(tokio::time::timeout(SETTLE, feeder.wait_until_running())
            .await
            .is_err());
        assert!(tokio::time::timeout(SETTLE, emitter.wait_until_running())
            .await
            .is_err());

        control.resume();

        assert_eq!(feeder.wait_until_running().await, Resumption::Proceed);
        assert_eq!(emitter.wait_until_running().await, Resumption::Proceed);
    }

    /// **The regression of the progress sampler.** A reporter that reads the
    /// control rather than assuming `Running` is the only thing that keeps a
    /// paused campaign showing as paused on the screen.
    #[tokio::test]
    async fn the_reported_status_follows_the_command_in_force() {
        use crate::campaign::CampaignStatus;

        let control = CampaignControl::new();

        assert_eq!(
            CampaignStatus::from(control.state()),
            CampaignStatus::Running
        );

        control.pause();
        assert_eq!(
            CampaignStatus::from(control.state()),
            CampaignStatus::Paused
        );

        control.resume();
        assert_eq!(
            CampaignStatus::from(control.state()),
            CampaignStatus::Running
        );

        control.cancel();
        assert_eq!(
            CampaignStatus::from(control.state()),
            CampaignStatus::Cancelled,
            "a campaign draining after a cancellation is not running"
        );
    }
}
