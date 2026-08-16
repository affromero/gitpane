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
use std::time::Duration;
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
    /// When under tmux (and `sleep_when_hidden` is on), a background probe
    /// tracks whether this pane is visible and the session recently touched.
    /// Anything but [`PowerState::Awake`] disables the three periodic timers
    /// at the source, so a hidden or idle instance schedules no timer wakeups
    /// at all. On wake, `Skip` makes tick/local fire exactly once immediately
    /// (instant refresh); the fetch timer is instead `reset()` so waking never
    /// triggers a surprise fetch of every repo.
    fn start_event_loop(&mut self) {
        let tick_rate = self.tick_rate;
        let poll_local = self.poll_local_interval;
        let poll_fetch = self.poll_fetch_interval;
        let doze_after = self.doze_after;
        let probe_pane = self
            .sleep_when_hidden
            .then(|| std::env::var("TMUX_PANE").ok())
            .flatten();
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

            let mut power_rx = spawn_visibility_probe(probe_pane, doze_after, token.clone());
            let mut power = PowerState::Awake;
            let mut reset_fetch = false;

            let _ = event_tx.send(Event::Init);

            loop {
                if reset_fetch {
                    fetch_timer.reset();
                    reset_fetch = false;
                }
                let awake = power == PowerState::Awake;
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
                        power = state;
                        let _ = event_tx.send(Event::Power(state));
                    }
                    _ = tick_delay, if awake => {
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
                        match event {
                            CrosstermEvent::Key(key) if key.kind == KeyEventKind::Press => {
                                let _ = event_tx.send(Event::Key(key));
                            }
                            CrosstermEvent::Mouse(mouse) => {
                                let _ = event_tx.send(Event::Mouse(mouse));
                            }
                            CrosstermEvent::Paste(text) => {
                                let _ = event_tx.send(Event::Paste(text));
                            }
                            CrosstermEvent::Resize(w, h) => {
                                let _ = event_tx.send(Event::Resize(w, h));
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
/// [`PowerState`] transitions. With no pane (outside tmux, or the feature
/// disabled) the sender is dropped immediately, so the receiver never yields
/// and the event loop stays permanently awake. The probe subprocess runs on a
/// blocking thread so a stalled tmux never delays input handling.
fn spawn_visibility_probe(
    pane: Option<String>,
    doze_after: Duration,
    token: CancellationToken,
) -> UnboundedReceiver<PowerState> {
    let (tx, rx) = mpsc::unbounded_channel();
    let Some(pane) = pane else {
        return rx;
    };
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(VISIBILITY_PROBE_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut last = PowerState::Awake;
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
            if state != last {
                last = state;
                if tx.send(state).is_err() {
                    break;
                }
            }
        }
    });
    rx
}
