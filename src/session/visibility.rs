//! Decides how much background work this instance should do. Under tmux the
//! verdict combines pane visibility with recent client activity; outside tmux
//! (when `sleep_when_hidden` is on) an input-idle probe in `tui.rs` drives
//! the state from input events alone, escalating idle straight to `DeepSleep`
//! since idleness is the only signal available there. Probe or parse failure
//! fails open to `Awake`.
use std::time::Duration;
/// How much background work the instance should do right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum PowerState {
    /// Pane visible and the user recently typed in the session: full polling.
    Awake,
    /// Pane visible but the session has been input-idle: no periodic polls or
    /// fetches, but filesystem-watcher refreshes still land so the display
    /// stays current when repos actually change.
    Doze,
    /// Pane hidden (detached session, background window, or zoomed away):
    /// nothing runs; one refresh fires on wake.
    DeepSleep,
}

/// tmux format string for [`parse_power_state`]:
/// `session_attached,window_active,pane_active,window_zoomed_flag,client_activity`.
pub(crate) const PROBE_FORMAT: &str =
    "#{session_attached},#{window_active},#{pane_active},#{window_zoomed_flag},#{client_activity}";

/// How long one probe may run before we kill it. A healthy tmux answers in
/// milliseconds; a wedged socket must not pile up stuck blocking threads.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// One `tmux display-message` roundtrip for `pane`. `None` when tmux is
/// absent, errors, or hangs past [`PROBE_TIMEOUT`] (the hung process is
/// killed). Blocking — call from `spawn_blocking`.
pub(crate) fn probe(pane: &str) -> Option<String> {
    use std::io::Read;
    use wait_timeout::ChildExt;

    let mut child = std::process::Command::new("tmux")
        .args(["display-message", "-p", "-t", pane, PROBE_FORMAT])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    match child.wait_timeout(PROBE_TIMEOUT) {
        Ok(Some(status)) if status.success() => {
            let mut out = String::new();
            child.stdout.take()?.read_to_string(&mut out).ok()?;
            Some(out.trim().to_string())
        }
        Ok(Some(_)) | Err(_) => None,
        Ok(None) => {
            let _ = child.kill();
            let _ = child.wait();
            None
        }
    }
}

/// Interpret one probe output line. `now_epoch` is the current UNIX time in
/// seconds; `doze_after` is how long the session may go without input before a
/// visible pane drops to [`PowerState::Doze`]. `None` only when the four
/// server-side fields are malformed — the caller must fail open to `Awake` so
/// a tmux quirk never freezes the UI.
///
/// `client_activity` is client-scoped: with no client attached anywhere on
/// the server (the terminal app was quit) tmux expands it to empty. So the
/// visibility verdict must come first, from the server-side fields alone —
/// an invisible pane deep-sleeps no matter what the activity field holds,
/// and a visible pane with an unreadable activity stays awake.
pub(crate) fn parse_power_state(
    output: &str,
    now_epoch: u64,
    doze_after: Duration,
) -> Option<PowerState> {
    let mut fields = output.trim().split(',');
    let mut next = || fields.next()?.trim().parse::<u64>().ok();
    let session_attached = next()?;
    let window_active = next()?;
    let pane_active = next()?;
    let window_zoomed = next()?;

    let zoomed_away = window_zoomed == 1 && pane_active == 0;
    let visible = session_attached >= 1 && window_active == 1 && !zoomed_away;
    if !visible {
        return Some(PowerState::DeepSleep);
    }
    let idle = match next() {
        Some(client_activity) => now_epoch.saturating_sub(client_activity) >= doze_after.as_secs(),
        None => false,
    };
    Some(if idle {
        PowerState::Doze
    } else {
        PowerState::Awake
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOZE: Duration = Duration::from_secs(120);
    const NOW: u64 = 10_000;

    #[test]
    fn visible_with_recent_input_is_awake() {
        let s = parse_power_state("1,1,1,0,9990", NOW, DOZE);
        assert_eq!(s, Some(PowerState::Awake));
    }

    #[test]
    fn visible_but_input_idle_dozes() {
        // Last input exactly at the threshold counts as idle.
        let s = parse_power_state("1,1,0,0,9880", NOW, DOZE);
        assert_eq!(s, Some(PowerState::Doze));
    }

    #[test]
    fn detached_session_deep_sleeps_even_with_recent_input() {
        let s = parse_power_state("0,1,1,0,9999", NOW, DOZE);
        assert_eq!(s, Some(PowerState::DeepSleep));
    }

    #[test]
    fn background_window_deep_sleeps() {
        let s = parse_power_state("1,0,1,0,9999", NOW, DOZE);
        assert_eq!(s, Some(PowerState::DeepSleep));
    }

    #[test]
    fn zoomed_sibling_pane_hides_us() {
        // Window is current but another pane is zoomed over ours.
        let s = parse_power_state("1,1,0,1,9999", NOW, DOZE);
        assert_eq!(s, Some(PowerState::DeepSleep));
    }

    #[test]
    fn zoomed_self_stays_awake() {
        let s = parse_power_state("1,1,1,1,9999", NOW, DOZE);
        assert_eq!(s, Some(PowerState::Awake));
    }

    #[test]
    fn multiple_attached_clients_count_as_attached() {
        let s = parse_power_state("2,1,1,0,9999", NOW, DOZE);
        assert_eq!(s, Some(PowerState::Awake));
    }

    #[test]
    fn clock_skew_ahead_of_now_is_not_idle() {
        // client_activity in the future (clock skew) must not underflow.
        let s = parse_power_state("1,1,1,0,10500", NOW, DOZE);
        assert_eq!(s, Some(PowerState::Awake));
    }

    #[test]
    fn no_clients_anywhere_still_deep_sleeps() {
        // With zero clients on the server (terminal app quit), tmux expands
        // the client-scoped activity field to empty. The visibility verdict
        // must not depend on it.
        assert_eq!(
            parse_power_state("0,1,0,0,", NOW, DOZE),
            Some(PowerState::DeepSleep)
        );
    }

    #[test]
    fn visible_with_unreadable_activity_stays_awake() {
        // Fail open: never doze off the back of a field we could not read.
        assert_eq!(
            parse_power_state("1,1,1,0,", NOW, DOZE),
            Some(PowerState::Awake)
        );
        assert_eq!(
            parse_power_state("1,1,1,0", NOW, DOZE),
            Some(PowerState::Awake)
        );
    }

    #[test]
    fn garbage_server_fields_fail_open_to_none() {
        assert_eq!(parse_power_state("", NOW, DOZE), None);
        assert_eq!(parse_power_state("no tmux server", NOW, DOZE), None);
        assert_eq!(parse_power_state("1,x,1,0,9999", NOW, DOZE), None);
        assert_eq!(parse_power_state("1,1", NOW, DOZE), None);
    }
}
