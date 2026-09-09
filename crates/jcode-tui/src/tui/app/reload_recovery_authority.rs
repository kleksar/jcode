//! Ephemeral client authority. History, interruption and reload files are data.
use super::{App, DisplayMessage};
use crate::protocol::ReloadRecoverySnapshot;

const HANDOFF_ENV: &str = "JCODE_RELOAD_RECOVERY_SESSION";

pub(super) fn take_reload_recovery_session_from_env(
    resume_session: Option<&str>,
) -> Option<String> {
    let handoff = std::env::var(HANDOFF_ENV).ok();
    // Remove even malformed, mismatched or unused handoffs. Never persist this.
    crate::env::remove_var(HANDOFF_ENV);
    handoff.filter(|session| !session.is_empty() && Some(session.as_str()) == resume_session)
}

impl App {
    pub(super) fn authorize_reload_recovery(&mut self, session_id: &str) {
        self.reload_recovery_authorized_session =
            (!session_id.is_empty()).then(|| session_id.to_owned());
    }

    pub(super) fn reload_recovery_is_authorized(&self, session_id: &str) -> bool {
        !session_id.is_empty()
            && self.reload_recovery_authorized_session.as_deref() == Some(session_id)
    }

    pub(super) fn admit_reload_recovery(
        &mut self,
        session_id: &str,
        directive: ReloadRecoverySnapshot,
    ) -> bool {
        if !self.reload_recovery_is_authorized(session_id) {
            return false;
        }
        // Admission, not dispatch, consumes authority. A failed write retains
        // its existing retry payload and duplicate History cannot mint another.
        self.reload_recovery_authorized_session = None;
        let continuation = directive.continuation_message;
        if let Some(notice) = directive.reconnect_notice
            && !self.reload_info.contains(&notice)
        {
            self.reload_info.push(notice);
        }
        let pending = self.hidden_queued_system_messages.contains(&continuation)
            || self
                .rate_limit_pending_message
                .as_ref()
                .and_then(|pending| pending.system_reminder.as_ref())
                .is_some_and(|message| message == &continuation);
        if !pending {
            self.push_display_message(DisplayMessage::system(
                "Reload complete - continuing because a recovery directive was pending."
                    .to_string(),
            ));
            self.hidden_queued_system_messages.push(continuation);
        }
        true
    }

    pub(super) fn take_reload_recovery_handoff(
        &mut self,
        reload_session: Option<&str>,
    ) -> Option<String> {
        self.reload_recovery_authorized_session
            .take()
            .filter(|session| !session.is_empty() && Some(session.as_str()) == reload_session)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resume_authority_env_exact_match_consumes_even_mismatch_and_empty() {
        let _env = crate::storage::lock_test_env();
        let previous = std::env::var_os(HANDOFF_ENV);
        for (value, target, expected) in [
            ("A", Some("A"), Some("A")),
            ("A", Some("B"), None),
            ("A", None, None),
            ("", Some(""), None),
            (" A", Some("A"), None),
        ] {
            crate::env::set_var(HANDOFF_ENV, value);
            assert_eq!(
                take_reload_recovery_session_from_env(target).as_deref(),
                expected
            );
            assert!(std::env::var_os(HANDOFF_ENV).is_none());
            assert!(take_reload_recovery_session_from_env(target).is_none());
        }
        if let Some(previous) = previous {
            crate::env::set_var(HANDOFF_ENV, previous);
        }
    }
}
