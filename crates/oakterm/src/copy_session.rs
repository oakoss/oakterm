//! Copy-mode coordination decisions: the bookkeeping that says which
//! pane a daemon reply belongs to and what an input event does while a
//! pane is modal. Pure — nothing here touches `App`, a window, or the
//! socket, so the rules are testable without an event loop, which the
//! GUI's own state is not.

use std::collections::HashMap;

/// The `YankSelection` requests awaiting their text, by pane. The daemon
/// answers with text alone, so the pane has to be remembered against the
/// serial; keying by pane is what makes "at most one pending yank per
/// pane" an invariant of the map rather than a rule every call site has
/// to keep.
#[derive(Debug, Default)]
pub(crate) struct PendingYanks(HashMap<u32, PendingYank>);

#[derive(Debug)]
struct PendingYank {
    serial: u32,
    /// Local half of a boundary-spanning yank (ADR-0025 clause 7),
    /// appended after the daemon half with a joining newline.
    tail: Option<String>,
}

/// Which pane a yank reply settles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum YankOutcome {
    /// No pending yank carries the serial: a superseded reply, or one for
    /// a yank already retired.
    Unclaimed,
    /// The pane whose pending yank the serial answers.
    Retire(u32),
}

impl PendingYanks {
    /// Note a yank sent for `pane_id`, superseding that pane's earlier
    /// request and no other pane's.
    pub(crate) fn record(&mut self, pane_id: u32, serial: u32) {
        self.0.insert(pane_id, PendingYank { serial, tail: None });
    }

    /// Note a boundary-spanning yank: the daemon serves rows below 0 and
    /// `tail` is the locally-extracted painted-page half.
    pub(crate) fn record_stitched(&mut self, pane_id: u32, serial: u32, tail: String) {
        self.0.insert(
            pane_id,
            PendingYank {
                serial,
                tail: Some(tail),
            },
        );
    }

    /// The stitch tail recorded for this pane's pending yank, consumed.
    pub(crate) fn take_tail(&mut self, pane_id: u32) -> Option<String> {
        self.0.get_mut(&pane_id)?.tail.take()
    }

    /// Which pane a reply at `serial` answers. The caller retires the
    /// entry once it has decided what to do with the text.
    pub(crate) fn resolve(&self, serial: u32) -> YankOutcome {
        self.0
            .iter()
            .find(|&(_, pending)| pending.serial == serial)
            .map_or(YankOutcome::Unclaimed, |(&pane_id, _)| {
                YankOutcome::Retire(pane_id)
            })
    }

    pub(crate) fn retire(&mut self, pane_id: u32) {
        self.0.remove(&pane_id);
    }
}

/// A copy-mode entry awaiting its ack (ADR-0025 clause 2), holding the
/// initial fill until the daemon confirms the pin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PendingEnter {
    pub(crate) pane_id: u32,
    pub(crate) fill: crate::copy_mode::FillRequest,
}

/// `EnterCopyMode` requests awaiting their acks, by serial. Recording an
/// entry supersedes any older one for the same pane, so a stale ack can
/// never release a previous session's fill into a newer session.
#[derive(Debug, Default)]
pub(crate) struct PendingEnters(HashMap<u32, PendingEnter>);

impl PendingEnters {
    pub(crate) fn record(
        &mut self,
        serial: u32,
        pane_id: u32,
        fill: crate::copy_mode::FillRequest,
    ) {
        self.0.retain(|_, pending| pending.pane_id != pane_id);
        self.0.insert(serial, PendingEnter { pane_id, fill });
    }

    /// Claim the entry the ack (or error) at `serial` answers.
    pub(crate) fn take(&mut self, serial: u32) -> Option<PendingEnter> {
        self.0.remove(&serial)
    }
}

/// What a matched yank reply leaves the GUI to do, once the clipboard
/// has answered (Spec-0008 Yank).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum YankDisposition {
    /// The text landed and the pane is still reading: leave copy mode.
    Exit,
    /// The text landed, but a scroll or resize tore copy mode down while
    /// the yank was in flight, so there is no mode left to leave.
    Done,
    /// The clipboard refused the text: hold the pane in copy mode with
    /// its selection so `y` can be pressed again, and ring — the key
    /// would otherwise look dead.
    HoldAndRing,
    /// The clipboard refused it and the pane has left copy mode anyway:
    /// nothing to hold, and no dead key to report.
    Failed,
}

/// Decide a matched yank's outcome. Emptiness is not a case: the text is
/// written whatever it says, so a blank selection is an ordinary copy.
pub(crate) fn yank_disposition(copied: bool, in_copy_mode: bool) -> YankDisposition {
    match (copied, in_copy_mode) {
        (true, true) => YankDisposition::Exit,
        (true, false) => YankDisposition::Done,
        (false, true) => YankDisposition::HoldAndRing,
        (false, false) => YankDisposition::Failed,
    }
}

/// What becomes of the key a leader press buffered when its follow-up
/// window closes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LeaderFlush {
    /// Send it to the PTY: the leader was a plain keypress after all
    /// (ADR-0011).
    Send(Vec<u8>),
    /// Copy mode owns the pane and forwards nothing (Spec-0008), so the
    /// key is consumed rather than typed behind the modal reader.
    Drop,
    /// Nothing was buffered.
    Nothing,
}

/// Decide a pending leader's fate. `copy_mode` is the state at flush
/// time, not at arm time: a pane that left copy mode inside the window
/// still gets its key, matching the miss path.
pub(crate) fn plan_leader_flush(buffered: Option<Vec<u8>>, copy_mode: bool) -> LeaderFlush {
    let Some(bytes) = buffered else {
        return LeaderFlush::Nothing;
    };
    if copy_mode {
        return LeaderFlush::Drop;
    }
    LeaderFlush::Send(bytes)
}

#[cfg(test)]
mod tests {
    use super::{
        LeaderFlush, PendingEnter, PendingEnters, PendingYanks, YankDisposition, YankOutcome,
        plan_leader_flush, yank_disposition,
    };

    /// Yanks are matched by serial and retire one pane's entry. A single
    /// global slot loses the race: a `y` on a second pane would overwrite
    /// the first pane's, leaving its reply unclaimable and the pane stuck
    /// in copy mode.
    #[test]
    fn a_yank_reply_retires_only_its_own_panes_entry() {
        let mut pending = PendingYanks::default();
        pending.record(4, 11);
        pending.record(9, 12);

        assert_eq!(pending.resolve(11), YankOutcome::Retire(4));
        assert_eq!(pending.resolve(12), YankOutcome::Retire(9));

        // Pane 9's reply lands first; pane 4's still completes normally.
        pending.retire(9);
        assert_eq!(pending.resolve(11), YankOutcome::Retire(4));
        assert_eq!(pending.resolve(12), YankOutcome::Unclaimed);
    }

    /// A reply nothing is waiting for, and one a newer `y` on the same
    /// pane superseded, are both ignored rather than exiting copy mode
    /// on text the user did not ask for.
    #[test]
    fn a_stale_or_unexpected_yank_reply_is_ignored() {
        assert_eq!(
            PendingYanks::default().resolve(11),
            YankOutcome::Unclaimed,
            "nothing outstanding"
        );

        let mut pending = PendingYanks::default();
        pending.record(4, 11);
        pending.record(4, 12);

        assert_eq!(pending.resolve(11), YankOutcome::Unclaimed, "superseded");
        assert_eq!(pending.resolve(12), YankOutcome::Retire(4));
    }

    /// Every path that ends a leader's follow-up window flushes through
    /// one decision, so none of them can type into the shell behind a
    /// modal reader — the miss path alone was not enough, since the
    /// timeout and a config reload flush too.
    #[test]
    fn a_buffered_leader_key_is_dropped_in_copy_mode_and_sent_outside_it() {
        let pending = |bytes: &[u8]| Some(bytes.to_vec());

        assert_eq!(
            plan_leader_flush(pending(b"\x02"), false),
            LeaderFlush::Send(b"\x02".to_vec())
        );
        assert_eq!(plan_leader_flush(pending(b"\x02"), true), LeaderFlush::Drop);
        assert_eq!(
            plan_leader_flush(None, false),
            LeaderFlush::Nothing,
            "a leader with no PTY encoding"
        );
        assert_eq!(plan_leader_flush(None, true), LeaderFlush::Nothing);
    }

    /// The yank's two independent facts: whether the clipboard took the
    /// text, and whether the pane is still in copy mode when the reply
    /// lands (a scroll or resize can tear it down mid-flight). Only the
    /// still-modal failure is worth a bell — the rest have no dead key
    /// to explain.
    #[test]
    fn a_yank_disposition_covers_both_failures_and_the_torn_down_pane() {
        assert_eq!(yank_disposition(true, true), YankDisposition::Exit);
        assert_eq!(yank_disposition(true, false), YankDisposition::Done);
        assert_eq!(yank_disposition(false, true), YankDisposition::HoldAndRing);
        assert_eq!(yank_disposition(false, false), YankDisposition::Failed);
    }

    /// The stitch tail comes back exactly once and dies with the entry:
    /// retirement or a superseding record must not leak it into a later
    /// yank's join.
    #[test]
    fn a_stitch_tail_is_consumed_once_and_dies_with_the_entry() {
        let mut pending = PendingYanks::default();
        pending.record_stitched(4, 11, "tail".to_string());

        assert_eq!(pending.resolve(11), YankOutcome::Retire(4));
        assert_eq!(pending.take_tail(4), Some("tail".to_string()));
        assert_eq!(pending.take_tail(4), None, "consumed once");

        pending.record_stitched(4, 12, "next".to_string());
        pending.record(4, 13);
        assert_eq!(pending.take_tail(4), None, "superseding record drops it");

        pending.record_stitched(4, 14, "gone".to_string());
        pending.retire(4);
        assert_eq!(pending.take_tail(4), None, "retirement drops it");
    }

    /// Recording an entry supersedes the pane's older one, so a stale
    /// ack can never release a previous session's fill into a newer
    /// session (ADR-0025 clause 2).
    #[test]
    fn a_new_enter_supersedes_the_panes_older_pending_entry() {
        let fill = crate::copy_mode::FillRequest {
            start_row: -24,
            count: 24,
        };
        let mut pending = PendingEnters::default();
        pending.record(11, 4, fill);
        pending.record(12, 4, fill);
        pending.record(13, 9, fill);

        assert_eq!(pending.take(11), None, "superseded by serial 12");
        assert_eq!(pending.take(12), Some(PendingEnter { pane_id: 4, fill }));
        assert_eq!(
            pending.take(13).map(|p| p.pane_id),
            Some(9),
            "other panes untouched"
        );
    }

    /// Leaving copy mode retires the pane's yank, so a reply that was
    /// already in flight resolves to nothing: no clipboard write the
    /// user has stopped waiting for, and no exit of whatever session
    /// they started next.
    #[test]
    fn leaving_copy_mode_strands_a_yank_that_was_still_in_flight() {
        let mut pending = PendingYanks::default();
        pending.record(4, 11);

        pending.retire(4);

        assert_eq!(pending.resolve(11), YankOutcome::Unclaimed);
    }
}
