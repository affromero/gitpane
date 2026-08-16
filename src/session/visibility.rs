//! Decides how much background work this instance should do based on whether
//! its tmux pane can currently be seen and whether the user is actively at the
//! terminal. tmux-only: outside tmux (or on any probe/parse failure) the
//! answer is `Awake`, i.e. behavior is unchanged.

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

/// One `tmux display-message` roundtrip for `pane`. `None` when tmux is absent
/// or errors. Blocking (a few ms) — call from `spawn_blocking`.
pub(crate) fn probe(pane: &str) -> Option<String> {
    let output = std::process::Command::new("tmux")
        .args(["display-message", "-p", "-t", pane, PROBE_FORMAT])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Interpret one probe output line. `now_epoch` is the current UNIX time in
/// seconds; `doze_after` is how long the session may go without input before a
/// visible pane drops to [`PowerState::Doze`]. `None` on any parse failure —
/// the caller must fail open to `Awake` so a tmux quirk never freezes the UI.
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
    let client_activity = next()?;

    let zoomed_away = window_zoomed == 1 && pane_active == 0;
    let visible = session_attached >= 1 && window_active == 1 && !zoomed_away;
    if !visible {
        return Some(PowerState::DeepSleep);
    }
    let idle = now_epoch.saturating_sub(client_activity) >= doze_after.as_secs();
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
    fn garbage_and_short_output_fail_open_to_none() {
        assert_eq!(parse_power_state("", NOW, DOZE), None);
        assert_eq!(parse_power_state("no tmux server", NOW, DOZE), None);
        assert_eq!(parse_power_state("1,1,1,0", NOW, DOZE), None);
        assert_eq!(parse_power_state("1,x,1,0,9999", NOW, DOZE), None);
    }
}
