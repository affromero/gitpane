use color_eyre::Result;
use crossterm::{
    cursor,
    event::{
        DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
        EnableFocusChange, EnableMouseCapture, Event as CrosstermEvent, EventStream, KeyEventKind,
    },
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use futures::StreamExt;
use ratatui::backend::CrosstermBackend;
use std::io::{self, Stdout, stdout};
use std::time::{Duration, Instant};
use tokio::{
    sync::mpsc::{self, UnboundedReceiver, UnboundedSender},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

use crate::event::Event;
use crate::session::visibility::{self, PowerState};

pub(crate) type Terminal = ratatui::Terminal<CrosstermBackend<Stdout>>;

/// How often the tmux visibility probe runs. Short enough that switching to a
/// sleeping pane wakes it within a beat; one probe is a single ~ms tmux
/// socket roundtrip, so this costs nothing next to a status poll.
const VISIBILITY_PROBE_INTERVAL: Duration = Duration::from_secs(3);

/// Tick rate while dozing. The app's run loop only drains finished background
/// work (status results, fetch completions) when an event arrives, so a
/// visible-but-idle pane needs a slow heartbeat or watcher-triggered results
/// would sit unrendered until the next keypress. One wakeup per second is
/// noise power-wise; deep sleep has no tick at all.
const DOZE_TICK_INTERVAL: Duration = Duration::from_secs(1);

pub(crate) struct Tui {
    pub terminal: Terminal,
    pub event_tx: UnboundedSender<Event>,
    pub event_rx: UnboundedReceiver<Event>,
    task: Option<JoinHandle<()>>,
    cancellation_token: CancellationToken,
    tick_rate: Duration,
    poll_local_interval: Duration,
    poll_fetch_interval: Duration,
    /// Gate periodic work on tmux pane visibility/idleness (see
    /// [`visibility`]). Outside tmux this has no effect.
    sleep_when_hidden: bool,
    /// Input-idle time before a visible pane stops polling.
    doze_after: Duration,
    mouse: bool,
}

impl Tui {
    pub fn new() -> Result<Self> {
        let backend = CrosstermBackend::new(stdout());
        let terminal = ratatui::Terminal::new(backend)?;
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        Ok(Self {
            terminal,
            event_tx,
            event_rx,
            task: None,
            cancellation_token: CancellationToken::new(),
            tick_rate: Duration::from_millis(250),
            poll_local_interval: Duration::from_secs(5),
            poll_fetch_interval: Duration::from_secs(60),
            sleep_when_hidden: true,
            doze_after: Duration::from_secs(120),
            mouse: false,
        })
    }

    #[allow(dead_code)]
    pub fn mouse(mut self, mouse: bool) -> Self {
        self.mouse = mouse;
        self
    }

    pub fn poll_local_interval(mut self, interval: Duration) -> Self {
        self.poll_local_interval = interval;
        self
    }

    pub fn poll_fetch_interval(mut self, interval: Duration) -> Self {
        self.poll_fetch_interval = interval;
        self
    }

    pub fn sleep_when_hidden(mut self, enabled: bool) -> Self {
        self.sleep_when_hidden = enabled;
        self
    }

    pub fn doze_after(mut self, idle: Duration) -> Self {
        self.doze_after = idle;
        self
    }

    pub fn enter(&mut self) -> Result<()> {
        // Fresh token so `enter()` after `exit()` (inline-command suspend)
        // restarts a live event loop rather than one that sees the previous
        // `exit()`'s cancellation and stops immediately.
        self.cancellation_token = CancellationToken::new();
        let mouse = self.mouse;
        enable_raw_mode()?;
        let setup = (|| -> io::Result<()> {
            crossterm::execute!(
                io::stdout(),
                EnterAlternateScreen,
                EnableBracketedPaste,
                EnableFocusChange,
            )?;
            if mouse {
                crossterm::execute!(io::stdout(), EnableMouseCapture)?;
            }
            Ok(())
        })();
        if let Err(e) = setup {
            // A later step failed after raw mode was enabled — roll back so we
            // don't strand the terminal in raw mode / the alternate screen.
            let _ = crossterm::execute!(
                io::stdout(),
                LeaveAlternateScreen,
                DisableBracketedPaste,
                DisableFocusChange,
                cursor::Show,
            );
            let _ = disable_raw_mode();
            return Err(e.into());
        }

        self.install_panic_hook();
        self.start_event_loop();
        Ok(())
    }

    pub fn exit(&mut self) -> Result<()> {
        self.cancellation_token.cancel();
        if let Some(task) = self.task.take() {
            task.abort();
        }
        // Best-effort teardown: attempt every step even if an earlier one fails,
        // so the terminal is restored as fully as possible (important for the
        // inline-suspend path, which re-enters afterward). Return the first error.
        let mut first_err: Option<std::io::Error> = None;
        if crossterm::terminal::is_raw_mode_enabled().unwrap_or(false) {
            if self.mouse
                && let Err(e) = crossterm::execute!(io::stdout(), DisableMouseCapture)
            {
                first_err.get_or_insert(e);
            }
            if let Err(e) = crossterm::execute!(
                io::stdout(),
                LeaveAlternateScreen,
                DisableBracketedPaste,
                DisableFocusChange,
                cursor::Show,
            ) {
                first_err.get_or_insert(e);
            }
            if let Err(e) = disable_raw_mode() {
                first_err.get_or_insert(e);
            }
        }
        match first_err {
            Some(e) => Err(e.into()),
            None => Ok(()),
        }
    }

    fn install_panic_hook(&self) {
        // Install exactly once per process: `enter()` runs on every inline
        // suspend/resume, and re-wrapping the hook each time would nest them
        // unboundedly.
        static HOOK: std::sync::Once = std::sync::Once::new();
        HOOK.call_once(|| {
            let original_hook = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |panic_info| {
                let _ = disable_raw_mode();
                let _ = crossterm::execute!(
                    io::stdout(),
                    LeaveAlternateScreen,
                    DisableBracketedPaste,
                    DisableFocusChange,
                    DisableMouseCapture,
                    cursor::Show,
                );
                original_hook(panic_info);
            }));
        });
    }

    /// Render-on-demand event loop: renders after input and background events.
    /// Ticks are kept for lightweight housekeeping only.
    ///
    /// When `sleep_when_hidden` is on, a background probe drives the power
    /// state: under tmux it tracks whether this pane is visible and the
    /// session recently touched; outside tmux an input-idle probe watches key
    /// and mouse events instead, and escalates its idle verdict straight to
    /// [`PowerState::DeepSleep`] (Doze is never emitted without visibility
    /// information).
    /// [`PowerState::Doze`] disables local/fetch polling but keeps a slow
    /// [`DOZE_TICK_INTERVAL`] heartbeat so watcher-triggered results still
    /// render; [`PowerState::DeepSleep`] disables every timer, so a hidden
    /// instance schedules no timer wakeups at all. On wake, `Skip` makes
    /// tick/local fire exactly once immediately (instant refresh); the fetch
    /// timer is instead `reset()` so waking never triggers a surprise fetch
    /// of every repo.
    fn start_event_loop(&mut self) {
        let tick_rate = self.tick_rate;
        let poll_local = self.poll_local_interval;
        let poll_fetch = self.poll_fetch_interval;
        let doze_after = self.doze_after;
        let probe_pane = self
            .sleep_when_hidden
            .then(|| std::env::var("TMUX_PANE").ok())
            .flatten();
        // Outside tmux (herdr, plain terminals) there is no pane to probe, so
        // the input-idle fallback drives the power state instead — as long as
        // the feature is enabled. Each key / meaningful mouse event is
        // forwarded to the probe so it can wake the instance immediately.
        let (input_tx, input_rx) = if probe_pane.is_none() && self.sleep_when_hidden {
            let (tx, rx) = mpsc::unbounded_channel();
            (Some(tx), Some(rx))
        } else {
            (None, None)
        };
        let event_tx = self.event_tx.clone();
        let token = self.cancellation_token.clone();

        self.task = Some(tokio::spawn(async move {
            let mut reader = EventStream::new();
            let mut tick_interval = tokio::time::interval(tick_rate);
            let mut local_timer = tokio::time::interval(poll_local);
            let mut fetch_timer = tokio::time::interval(poll_fetch);
            // Skip, not the default Burst: a re-enabled timer fires once
            // immediately instead of replaying every tick it slept through.
            tick_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            local_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            fetch_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            let mut power_rx =
                spawn_visibility_probe(probe_pane, input_rx, doze_after, token.clone());
            let mut power = PowerState::Awake;
            let mut reset_fetch = false;
            let mut retune_tick: Option<Duration> = None;

            let _ = event_tx.send(Event::Init);

            loop {
                if reset_fetch {
                    fetch_timer.reset();
                    reset_fetch = false;
                }
                if let Some(rate) = retune_tick.take() {
                    tick_interval = tokio::time::interval(rate);
                    tick_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                }
                let awake = power == PowerState::Awake;
                // Doze keeps a slow tick so background results still render.
                let ticking = power != PowerState::DeepSleep;
                let tick_delay = tick_interval.tick();
                let local_delay = local_timer.tick();
                let fetch_delay = fetch_timer.tick();
                let crossterm_event = reader.next();

                tokio::select! {
                    _ = token.cancelled() => break,
                    Some(state) = power_rx.recv() => {
                        if state == PowerState::Awake && power != PowerState::Awake {
                            reset_fetch = true;
                        }
                        if state != power {
                            retune_tick = Some(if state == PowerState::Awake {
                                tick_rate
                            } else {
                                DOZE_TICK_INTERVAL
                            });
                        }
                        power = state;
                        let _ = event_tx.send(Event::Power(state));
                    }
                    _ = tick_delay, if ticking => {
                        let _ = event_tx.send(Event::Tick);
                    }
                    _ = local_delay, if awake => {
                        let _ = event_tx.send(Event::PollLocal);
                        let _ = event_tx.send(Event::Render);
                    }
                    _ = fetch_delay, if awake => {
                        let _ = event_tx.send(Event::PollFetch);
                        let _ = event_tx.send(Event::Render);
                    }
                    Some(Ok(event)) = crossterm_event => {
                        // Forward input to the non-tmux idle probe so it can
                        // wake the instance (no-op when not wired).
                        match &event {
                            CrosstermEvent::Key(key) if key.kind == KeyEventKind::Press => {
                                let _ = event_tx.send(Event::Key(*key));
                                if let Some(tx) = &input_tx {
                                    let _ = tx.send(());
                                }
                            }
                            CrosstermEvent::Mouse(mouse) => {
                                // Plain cursor moves should not count as the
                                // user actively working; everything else does.
                                let meaningful =
                                    !matches!(mouse.kind, crossterm::event::MouseEventKind::Moved);
                                if meaningful && let Some(tx) = &input_tx {
                                    let _ = tx.send(());
                                }
                                let _ = event_tx.send(Event::Mouse(*mouse));
                            }
                            CrosstermEvent::Paste(text) => {
                                let _ = event_tx.send(Event::Paste(text.clone()));
                            }
                            CrosstermEvent::Resize(w, h) => {
                                let _ = event_tx.send(Event::Resize(*w, *h));
                            }
                            CrosstermEvent::FocusGained => {
                                let _ = event_tx.send(Event::FocusGained);
                            }
                            CrosstermEvent::FocusLost => {
                                let _ = event_tx.send(Event::FocusLost);
                            }
                            _ => {}
                        }
                        // Render immediately after any user input
                        let _ = event_tx.send(Event::Render);
                    }
                }
            }
        }));
    }
}

impl Drop for Tui {
    fn drop(&mut self) {
        let _ = self.exit();
    }
}

/// Periodically probes tmux for this pane's visibility/idleness and emits
/// [`PowerState`] transitions. With no pane and no input channel (outside
/// tmux with the feature disabled) the sender is dropped immediately, so the
/// receiver never yields and the event loop stays permanently awake. The
/// probe subprocess runs on a blocking thread so a stalled tmux never delays
/// input handling.
///
/// Outside tmux, when `sleep_when_hidden` is enabled, `input_rx` is wired to
/// the event loop's key/mouse stream and an input-idle probe takes over:
/// every input resets the idle clock and wakes the instance immediately, and
/// after `doze_after` without input it drops to [`PowerState::Doze`]
/// (`DeepSleep` is unreachable without visibility information).
fn spawn_visibility_probe(
    pane: Option<String>,
    input_rx: Option<UnboundedReceiver<()>>,
    doze_after: Duration,
    token: CancellationToken,
) -> UnboundedReceiver<PowerState> {
    let (tx, rx) = mpsc::unbounded_channel();
    let Some(pane) = pane else {
        // Outside tmux: fall back to input-idle gating when the feature is on
        // (input_rx wired); otherwise drop the sender to stay always awake.
        let Some(input_rx) = input_rx else {
            return rx;
        };
        spawn_input_idle_probe(input_rx, doze_after, VISIBILITY_PROBE_INTERVAL, token, tx);
        return rx;
    };
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(VISIBILITY_PROBE_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // `None` so the first probe always delivers: after an inline-suspend
        // re-entry the consumers' state may be stale, and only an
        // unconditional first report is guaranteed to resync them.
        let mut last: Option<PowerState> = None;
        loop {
            tokio::select! {
                _ = token.cancelled() => break,
                _ = interval.tick() => {}
            }
            let pane_arg = pane.clone();
            let output = tokio::task::spawn_blocking(move || visibility::probe(&pane_arg))
                .await
                .ok()
                .flatten();
            let now_epoch = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            // Fail open: any probe or parse hiccup keeps full behavior.
            let state = output
                .and_then(|o| visibility::parse_power_state(&o, now_epoch, doze_after))
                .unwrap_or(PowerState::Awake);
            if last != Some(state) {
                last = Some(state);
                if tx.send(state).is_err() {
                    break;
                }
            }
        }
    });
    rx
}

/// Non-tmux fallback probe: there is no tmux pane to ask about visibility, so
/// the [`PowerState`] is driven purely by input idleness. Every event on
/// `input_rx` (a key press or a meaningful mouse action) marks the user active
/// and immediately wakes the instance; after `doze_after` without any input the
/// instance drops to [`PowerState::DeepSleep`] — everything pauses, including
/// watcher-driven refreshes, until the next input.
///
/// Outside tmux we cannot tell whether the pane is actually visible, so input
/// idleness is treated as "the user has left". Emitting `DeepSleep` (rather
/// than the tmux-style `Doze`, where the watcher keeps refreshing) is
/// deliberate: on large multi-repo workspaces the Doze semantics keep slow
/// status queries in-flight forever and dirty-replay re-queues them into a
/// self-sustaining refresh loop. DeepSleep gates the watcher too, breaking that
/// loop, and reuses upstream's existing wake-and-refresh semantics.
fn spawn_input_idle_probe(
    input_rx: UnboundedReceiver<()>,
    doze_after: Duration,
    probe_interval: Duration,
    token: CancellationToken,
    tx: UnboundedSender<PowerState>,
) {
    tokio::spawn(async move {
        // `None` so the first tick always delivers a state, resyncing any
        // stale consumer state after an inline-suspend re-entry.
        let mut last: Option<PowerState> = None;
        // Start the idle clock at launch: a fresh instance polls normally,
        // then DeepSleeps once `doze_after` elapses without any input. (`None`
        // would keep the instance awake forever, since `input_idle_state`
        // treats "no input recorded" as active.)
        let mut last_input: Option<Instant> = Some(Instant::now());
        let mut input_rx = input_rx;
        loop {
            tokio::select! {
                _ = token.cancelled() => break,
                Some(()) = input_rx.recv() => {
                    // Any input marks the user active and wakes immediately.
                    last_input = Some(Instant::now());
                    if last != Some(PowerState::Awake) {
                        last = Some(PowerState::Awake);
                        if tx.send(PowerState::Awake).is_err() {
                            break;
                        }
                    }
                }
                _ = tokio::time::sleep(probe_interval) => {
                    // Outside tmux there is no visibility signal: input idle
                    // means the user has left, not merely paused. Emit
                    // DeepSleep (not Doze) so the app gates watcher-driven
                    // refreshes too — on huge workspaces the Doze semantics
                    // (watcher keeps refreshing) would otherwise self-sustain
                    // a refresh loop, since slow status queries stay
                    // in-flight and dirty-replay keeps re-queuing them.
                    let state =
                        visibility::input_idle_state(last_input, Instant::now(), doze_after);
                    let state = match state {
                        PowerState::Doze => PowerState::DeepSleep,
                        other => other,
                    };
                    if last != Some(state) {
                        last = Some(state);
                        if tx.send(state).is_err() {
                            break;
                        }
                    }
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The input-idle probe emits `DeepSleep` after `doze_after` without
    /// input (outside tmux there is no visibility signal, so idle == user
    /// left), wakes to `Awake` immediately on input, and never emits `Doze`.
    #[tokio::test]
    async fn input_idle_probe_deep_sleeps_and_wakes_on_input() {
        let (input_tx, input_rx) = mpsc::unbounded_channel();
        let (state_tx, mut state_rx) = mpsc::unbounded_channel();
        let token = CancellationToken::new();

        spawn_input_idle_probe(
            input_rx,
            Duration::from_millis(100),
            Duration::from_millis(20),
            token.clone(),
            state_tx,
        );

        // No input ever arrives → the instance should drop to DeepSleep
        // within a few probe ticks, and never emit an intermediate Doze.
        let slept = tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                match state_rx.recv().await {
                    Some(PowerState::DeepSleep) => return PowerState::DeepSleep,
                    // Doze must never be emitted: outside tmux the probe
                    // remaps it to DeepSleep (see the fn docs).
                    Some(PowerState::Doze) => panic!("probe must not emit Doze"),
                    Some(_) => continue,
                    None => panic!("probe sender dropped"),
                }
            }
        })
        .await
        .expect("probe should emit DeepSleep after idleness");
        assert_eq!(slept, PowerState::DeepSleep);

        // A single input event wakes it back to Awake immediately (before the
        // next 100ms doze threshold).
        input_tx.send(()).unwrap();
        let wake = tokio::time::timeout(Duration::from_secs(1), state_rx.recv())
            .await
            .expect("probe should emit Awake on input")
            .expect("probe sender dropped");
        assert_eq!(wake, PowerState::Awake);

        token.cancel();
    }
}
