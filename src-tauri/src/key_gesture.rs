//! One-key dictation: hold the 🌐 fn key (or Right ⌥ Option) to talk, double-tap
//! it for hands-free dictation and tap once more to finish. While the key is held,
//! → and ← switch the output language; keep holding to dictate in the new one.
//!
//! The global-shortcut plugin cannot see a lone modifier, so on macOS an event tap
//! watches the key itself. It needs the Accessibility permission Murmela already
//! uses for pasting. `GestureMachine` holds the timing rules and is platform-free.
use std::time::{Duration, Instant};

/// Settings value for the 🌐 fn key.
pub const FN_KEY: &str = "Fn";
/// Settings value for the right-hand Option key.
pub const RIGHT_OPTION_KEY: &str = "RightAlt";

/// A press longer than this starts push-to-talk; a shorter one is a tap.
pub const HOLD_DELAY: Duration = Duration::from_millis(180);
/// Two taps closer than this start hands-free dictation.
pub const DOUBLE_TAP_WINDOW: Duration = Duration::from_millis(400);
/// After → or ←, push-to-talk starts once the key has been held this long
/// without another arrow, so a quick switch records nothing.
pub const SWITCH_HOLD_DELAY: Duration = Duration::from_millis(500);

/// Whether a dictation shortcut setting names a single key handled here.
pub fn is_key_gesture(shortcut: &str) -> bool {
    matches!(shortcut, FN_KEY | RIGHT_OPTION_KEY)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arrow {
    Next,
    Previous,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// The key is held: start push-to-talk.
    StartHold,
    /// Released after push-to-talk: transcribe and paste.
    StopHold,
    /// Double-tap: start hands-free dictation.
    StartHandsFree,
    /// Tap during hands-free dictation: transcribe and paste.
    StopHandsFree,
    /// Key + arrow: switch the output language.
    Cycle(Arrow),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Idle { last_tap: Option<Instant> },
    /// Down, not yet long enough to be a hold.
    Pressed { since: Instant, combo: bool },
    Holding,
    /// Down for the second tap of a double-tap; its release is not a tap.
    StartedHandsFree,
    HandsFree,
    /// Down for the tap that ended hands-free dictation.
    StoppingHandsFree,
    /// Held after → or ←; becomes push-to-talk in the new language after
    /// `SWITCH_HOLD_DELAY` from the last arrow. Its release is not a tap.
    Switched { since: Instant },
    /// Held after another key combination; nothing more happens until release.
    Combo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GestureMachine {
    state: State,
}

impl Default for GestureMachine {
    fn default() -> Self {
        Self {
            state: State::Idle { last_tap: None },
        }
    }
}

impl GestureMachine {
    /// The dictation key went down. `dictating` is true while a dictation runs,
    /// so a tap after an external stop (Esc, error) starts afresh.
    pub fn key_down(&mut self, now: Instant, dictating: bool) -> Option<Action> {
        match self.state {
            State::HandsFree if dictating => {
                self.state = State::StoppingHandsFree;
                Some(Action::StopHandsFree)
            }
            State::Idle { last_tap: Some(tap) } if now.duration_since(tap) <= DOUBLE_TAP_WINDOW => {
                self.state = State::StartedHandsFree;
                Some(Action::StartHandsFree)
            }
            _ => {
                self.state = State::Pressed {
                    since: now,
                    combo: false,
                };
                None
            }
        }
    }

    /// Call regularly while the key is down; starts push-to-talk after `HOLD_DELAY`.
    pub fn tick(&mut self, now: Instant) -> Option<Action> {
        match self.state {
            State::Pressed { since, combo: false } if now.duration_since(since) >= HOLD_DELAY => {
                self.state = State::Holding;
                Some(Action::StartHold)
            }
            State::Switched { since } if now.duration_since(since) >= SWITCH_HOLD_DELAY => {
                self.state = State::Holding;
                Some(Action::StartHold)
            }
            _ => None,
        }
    }

    pub fn key_up(&mut self, now: Instant) -> Option<Action> {
        let (next, action) = match self.state {
            State::Pressed { combo: false, .. } => (State::Idle { last_tap: Some(now) }, None),
            State::Holding => (State::Idle { last_tap: None }, Some(Action::StopHold)),
            State::StartedHandsFree => (State::HandsFree, None),
            State::HandsFree => (State::HandsFree, None),
            _ => (State::Idle { last_tap: None }, None),
        };
        self.state = next;
        action
    }

    /// Another key went down while the dictation key is held. Returns the actions
    /// and whether the key press should be kept from the focused app.
    pub fn other_key(&mut self, now: Instant, arrow: Option<Arrow>) -> (Vec<Action>, bool) {
        match (self.state, arrow) {
            (State::Pressed { .. } | State::Switched { .. }, Some(arrow)) => {
                self.state = State::Switched { since: now };
                (vec![Action::Cycle(arrow)], true)
            }
            (State::Pressed { since, .. }, None) => {
                // fn+Delete, fn+F-keys and the like: never a dictation.
                self.state = State::Pressed { since, combo: true };
                (vec![], false)
            }
            (State::Switched { .. }, None) => {
                self.state = State::Combo;
                (vec![], false)
            }
            // Push-to-talk keeps recording; the dictation takes the new language.
            (State::Holding, Some(arrow)) => (vec![Action::Cycle(arrow)], true),
            _ => (vec![], false),
        }
    }

    /// The key is held long enough that a tick is due.
    pub fn waiting_for_hold(&self) -> bool {
        matches!(self.state, State::Pressed { combo: false, .. } | State::Switched { .. })
    }
}

pub mod tap {
    //! Session event tap for the dictation key. Active (not listen-only) so the
    //! arrow of a language combo can be kept from the focused app.
    use super::{Action, Arrow, GestureMachine};
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    type CFMachPortRef = *mut c_void;
    type CFRunLoopSourceRef = *mut c_void;
    type CFRunLoopRef = *mut c_void;
    type CGEventRef = *mut c_void;
    type CGEventTapProxy = *mut c_void;
    type TapCallback = extern "C" fn(CGEventTapProxy, u32, CGEventRef, *mut c_void) -> CGEventRef;

    const SESSION_TAP: u32 = 1;
    const HEAD_INSERT: u32 = 0;
    const TAP_DEFAULT: u32 = 0;
    const KEY_DOWN: u32 = 10;
    const KEY_UP: u32 = 11;
    const FLAGS_CHANGED: u32 = 12;
    const TAP_DISABLED_BY_TIMEOUT: u32 = 0xFFFF_FFFE;
    const TAP_DISABLED_BY_USER: u32 = 0xFFFF_FFFF;
    const KEYCODE_FIELD: u32 = 9;
    const AUTOREPEAT_FIELD: u32 = 8;
    const FLAG_SECONDARY_FN: u64 = 0x0080_0000;
    const FLAG_RIGHT_OPTION: u64 = 0x0000_0040;
    const KEY_FN: i64 = 63;
    const KEY_GLOBE: i64 = 179;
    const KEY_RIGHT_OPTION: i64 = 61;
    // fn+→/← on Mac keyboards arrive as End/Home.
    const KEY_RIGHT: [i64; 2] = [124, 119];
    const KEY_LEFT: [i64; 2] = [123, 115];

    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn CGEventTapCreate(
            tap: u32,
            place: u32,
            options: u32,
            events_of_interest: u64,
            callback: TapCallback,
            user_info: *mut c_void,
        ) -> CFMachPortRef;
        fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);
        fn CGEventGetFlags(event: CGEventRef) -> u64;
        fn CGEventGetIntegerValueField(event: CGEventRef, field: u32) -> i64;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        static kCFRunLoopCommonModes: *const c_void;
        fn CFMachPortCreateRunLoopSource(
            allocator: *const c_void,
            port: CFMachPortRef,
            order: isize,
        ) -> CFRunLoopSourceRef;
        fn CFRunLoopGetCurrent() -> CFRunLoopRef;
        fn CFRunLoopAddSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: *const c_void);
        fn CFRunLoopRun();
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    pub enum Key {
        Fn,
        RightOption,
    }

    /// What the tap reports to the app.
    pub trait Sink: Send + Sync + 'static {
        fn act(&self, action: Action);
        fn dictating(&self) -> bool;
        /// Whether → / ← while the key is held should switch the language.
        fn language_combo_enabled(&self) -> bool;
        /// `true`: the key is down and may become push-to-talk, so the
        /// microphone can open now. `false`: it did not (a tap or another
        /// combination). Never called for a press that started dictating.
        /// Must not block: it runs on the event tap.
        fn prime(&self, _likely: bool) {}
    }

    struct Shared {
        key: Mutex<Option<Key>>,
        machine: Mutex<GestureMachine>,
        down: AtomicBool,
        /// Arrow key codes whose key-up must be swallowed too.
        swallowed: Mutex<Vec<i64>>,
        tap: Mutex<usize>,
        sink: Box<dyn Sink>,
        /// Whether the sink was told a hold may follow.
        primed: AtomicBool,
    }

    impl Shared {
        /// Tells the sink whether the press may still become push-to-talk.
        fn prime(&self, waiting: bool) {
            let was = self.primed.swap(waiting, Ordering::SeqCst);
            if waiting != was {
                self.sink.prime(waiting);
            }
        }
    }

    /// The running tap. Change the watched key with `set_key`; `None` pauses it.
    pub struct KeyTap {
        shared: Arc<Shared>,
    }

    impl KeyTap {
        /// Starts the tap thread. Fails without the Accessibility permission.
        pub fn start(sink: impl Sink) -> anyhow::Result<Self> {
            let shared = Arc::new(Shared {
                key: Mutex::new(None),
                machine: Mutex::new(GestureMachine::default()),
                down: AtomicBool::new(false),
                swallowed: Mutex::new(Vec::new()),
                tap: Mutex::new(0),
                sink: Box::new(sink),
                primed: AtomicBool::new(false),
            });
            let (ready_tx, ready_rx) = std::sync::mpsc::channel();
            let context = Arc::into_raw(shared.clone()) as usize;
            std::thread::Builder::new()
                .name("murmela-key-tap".into())
                .spawn(move || unsafe {
                    let mask = (1u64 << KEY_DOWN) | (1u64 << KEY_UP) | (1u64 << FLAGS_CHANGED);
                    let tap = CGEventTapCreate(
                        SESSION_TAP,
                        HEAD_INSERT,
                        TAP_DEFAULT,
                        mask,
                        callback,
                        context as *mut c_void,
                    );
                    if tap.is_null() {
                        drop(Arc::from_raw(context as *const Shared));
                        let _ = ready_tx.send(false);
                        return;
                    }
                    (*(context as *const Shared)).tap.lock().map(|mut t| *t = tap as usize).ok();
                    let source = CFMachPortCreateRunLoopSource(std::ptr::null(), tap, 0);
                    CFRunLoopAddSource(CFRunLoopGetCurrent(), source, kCFRunLoopCommonModes);
                    CGEventTapEnable(tap, true);
                    let _ = ready_tx.send(true);
                    CFRunLoopRun();
                })?;
            if !ready_rx.recv_timeout(Duration::from_secs(5)).unwrap_or(false) {
                anyhow::bail!(
                    "KEY_TAP_UNAVAILABLE: Allow Murmela under Privacy & Security → Accessibility to dictate with the 🌐 or Right ⌥ key."
                );
            }
            let ticker = shared.clone();
            std::thread::Builder::new()
                .name("murmela-key-hold".into())
                .spawn(move || loop {
                    std::thread::sleep(Duration::from_millis(15));
                    if !ticker.down.load(Ordering::SeqCst) {
                        continue;
                    }
                    let action = ticker
                        .machine
                        .lock()
                        .ok()
                        .and_then(|mut machine| machine.waiting_for_hold().then(|| machine.tick(Instant::now())).flatten());
                    if let Some(action) = action {
                        // The dictation now owns the opened microphone.
                        ticker.primed.store(false, Ordering::SeqCst);
                        ticker.sink.act(action);
                    }
                })?;
            Ok(Self { shared })
        }

        pub fn set_key(&self, key: Option<Key>) {
            if let Ok(mut current) = self.shared.key.lock() {
                if *current != key {
                    *current = key;
                    self.shared.down.store(false, Ordering::SeqCst);
                    if let Ok(mut machine) = self.shared.machine.lock() {
                        *machine = GestureMachine::default();
                    }
                }
            }
        }
    }

    fn arrow_for(keycode: i64) -> Option<Arrow> {
        if KEY_RIGHT.contains(&keycode) {
            Some(Arrow::Next)
        } else if KEY_LEFT.contains(&keycode) {
            Some(Arrow::Previous)
        } else {
            None
        }
    }

    extern "C" fn callback(
        _proxy: CGEventTapProxy,
        kind: u32,
        event: CGEventRef,
        context: *mut c_void,
    ) -> CGEventRef {
        let shared = unsafe { &*(context as *const Shared) };
        if kind == TAP_DISABLED_BY_TIMEOUT || kind == TAP_DISABLED_BY_USER {
            if let Ok(tap) = shared.tap.lock() {
                unsafe { CGEventTapEnable(*tap as CFMachPortRef, true) };
            }
            return event;
        }
        let Some(key) = shared.key.lock().ok().and_then(|key| *key) else {
            return event;
        };
        let keycode = unsafe { CGEventGetIntegerValueField(event, KEYCODE_FIELD) };
        let now = Instant::now();
        match kind {
            FLAGS_CHANGED => {
                let flags = unsafe { CGEventGetFlags(event) };
                let pressed = match (key, keycode) {
                    (Key::Fn, KEY_FN | KEY_GLOBE) => Some(flags & FLAG_SECONDARY_FN != 0),
                    (Key::RightOption, KEY_RIGHT_OPTION) => Some(flags & FLAG_RIGHT_OPTION != 0),
                    _ => None,
                };
                let Some(pressed) = pressed else { return event };
                if pressed == shared.down.swap(pressed, Ordering::SeqCst) {
                    return event; // Repeated flag state.
                }
                let dictating = shared.sink.dictating();
                let (action, waiting) = shared
                    .machine
                    .lock()
                    .map(|mut machine| {
                        let action = if pressed {
                            machine.key_down(now, dictating)
                        } else {
                            machine.key_up(now)
                        };
                        (action, machine.waiting_for_hold())
                    })
                    .unwrap_or((None, false));
                shared.prime(waiting);
                if let Some(action) = action {
                    shared.sink.act(action);
                }
                event
            }
            KEY_DOWN if shared.down.load(Ordering::SeqCst) => {
                let repeat = unsafe { CGEventGetIntegerValueField(event, AUTOREPEAT_FIELD) } != 0;
                let arrow = arrow_for(keycode).filter(|_| shared.sink.language_combo_enabled());
                if repeat && arrow.is_some() {
                    return std::ptr::null_mut(); // Holding the arrow must not keep cycling.
                }
                let (actions, swallow, waiting) = shared
                    .machine
                    .lock()
                    .map(|mut machine| {
                        let (actions, swallow) = machine.other_key(now, arrow);
                        (actions, swallow, machine.waiting_for_hold())
                    })
                    .unwrap_or_default();
                shared.prime(waiting);
                for action in actions {
                    shared.sink.act(action);
                }
                if swallow {
                    if let Ok(mut list) = shared.swallowed.lock() {
                        list.push(keycode);
                    }
                    return std::ptr::null_mut();
                }
                event
            }
            KEY_UP => {
                if let Ok(mut list) = shared.swallowed.lock() {
                    if let Some(index) = list.iter().position(|code| *code == keycode) {
                        list.remove(index);
                        return std::ptr::null_mut();
                    }
                }
                event
            }
            _ => event,
        }
    }
}

/// What macOS does when the 🌐 key is pressed alone ("Press 🌐 key to").
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GlobeKeyAction {
    DoNothing,
    ChangeInputSource,
    ShowEmoji,
    StartDictation,
    Unknown,
}

/// Reads the "Press 🌐 key to" setting. Anything but "Do Nothing" makes macOS
/// react to a tap of the key too.
pub fn globe_key_action() -> GlobeKeyAction {
    let output = std::process::Command::new("/usr/bin/defaults")
        .args(["read", "com.apple.HIToolbox", "AppleFnUsageType"])
        .output();
    match output {
        Ok(output) if output.status.success() => {
            match String::from_utf8_lossy(&output.stdout).trim() {
                "0" => GlobeKeyAction::DoNothing,
                "1" => GlobeKeyAction::ChangeInputSource,
                "2" => GlobeKeyAction::ShowEmoji,
                "3" => GlobeKeyAction::StartDictation,
                _ => GlobeKeyAction::Unknown,
            }
        }
        // Never changed: the macOS default opens the emoji picker.
        Ok(_) => GlobeKeyAction::ShowEmoji,
        Err(_) => GlobeKeyAction::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(start: Instant, ms: u64) -> Instant {
        start + Duration::from_millis(ms)
    }

    #[test]
    fn holding_starts_after_the_delay_and_release_pastes() {
        let t = Instant::now();
        let mut m = GestureMachine::default();
        assert_eq!(m.key_down(t, false), None);
        assert_eq!(m.tick(at(t, 100)), None);
        assert_eq!(m.tick(at(t, 190)), Some(Action::StartHold));
        assert_eq!(m.tick(at(t, 400)), None);
        assert_eq!(m.key_up(at(t, 3_000)), Some(Action::StopHold));
    }

    #[test]
    fn a_single_tap_does_nothing_and_a_double_tap_goes_hands_free() {
        let t = Instant::now();
        let mut m = GestureMachine::default();
        assert_eq!(m.key_down(t, false), None);
        assert_eq!(m.key_up(at(t, 90)), None);
        // A late second tap is just another single tap.
        assert_eq!(m.key_down(at(t, 700), false), None);
        assert_eq!(m.key_up(at(t, 780)), None);
        assert_eq!(m.key_down(at(t, 980), false), Some(Action::StartHandsFree));
        assert_eq!(m.tick(at(t, 1_300)), None, "the second tap's hold is not push-to-talk");
        assert_eq!(m.key_up(at(t, 1_400)), None);
        // Hands-free continues until the next press.
        assert_eq!(m.key_down(at(t, 9_000), true), Some(Action::StopHandsFree));
        assert_eq!(m.key_up(at(t, 9_080)), None);
        assert_eq!(m.key_down(at(t, 9_300), false), None, "the stop tap does not count towards a double-tap");
    }

    #[test]
    fn a_tap_after_an_external_stop_starts_afresh() {
        let t = Instant::now();
        let mut m = GestureMachine::default();
        m.key_down(t, false);
        m.key_up(at(t, 50));
        assert_eq!(m.key_down(at(t, 200), false), Some(Action::StartHandsFree));
        m.key_up(at(t, 260));
        // Esc ended the dictation meanwhile.
        assert_eq!(m.key_down(at(t, 5_000), false), None);
        assert_eq!(m.tick(at(t, 5_200)), Some(Action::StartHold));
    }

    #[test]
    fn key_combinations_switch_language_or_pass_through() {
        let t = Instant::now();
        let mut m = GestureMachine::default();
        m.key_down(t, false);
        assert_eq!(m.other_key(at(t, 60), Some(Arrow::Next)), (vec![Action::Cycle(Arrow::Next)], true));
        assert_eq!(m.tick(at(t, 400)), None, "the hold waits after an arrow");
        assert_eq!(m.other_key(at(t, 450), Some(Arrow::Previous)), (vec![Action::Cycle(Arrow::Previous)], true));
        assert_eq!(m.tick(at(t, 900)), None, "each arrow restarts the wait");
        assert_eq!(m.key_up(at(t, 920)), None, "a quick switch dictates nothing");
        assert_eq!(m.key_down(at(t, 1_000), false), None, "a switch is not the first tap of a double-tap");

        let mut m = GestureMachine::default();
        m.key_down(t, false);
        assert_eq!(m.other_key(at(t, 50), None), (vec![], false), "fn+Delete reaches the app");
        assert_eq!(m.tick(at(t, 300)), None);
    }

    #[test]
    fn holding_on_after_a_switch_dictates_in_the_new_language() {
        let t = Instant::now();
        let mut m = GestureMachine::default();
        m.key_down(t, false);
        assert_eq!(m.other_key(at(t, 80), Some(Arrow::Next)), (vec![Action::Cycle(Arrow::Next)], true));
        assert_eq!(m.tick(at(t, 580)), Some(Action::StartHold));
        assert_eq!(m.key_up(at(t, 4_000)), Some(Action::StopHold));

        let mut m = GestureMachine::default();
        m.key_down(t, false);
        m.other_key(at(t, 80), Some(Arrow::Next));
        assert_eq!(m.other_key(at(t, 120), None), (vec![], false), "another key ends the gesture");
        assert_eq!(m.tick(at(t, 900)), None);
        assert_eq!(m.key_up(at(t, 1_000)), None);
    }

    #[test]
    fn arrows_while_dictating_switch_its_language_at_any_pace() {
        let t = Instant::now();
        let mut m = GestureMachine::default();
        m.key_down(t, false);
        assert_eq!(m.tick(at(t, 200)), Some(Action::StartHold));
        // Slow, steady presses: every one switches, none stops the dictation.
        for press in 1..=8 {
            assert_eq!(
                m.other_key(at(t, 200 + press * 1_500), Some(Arrow::Next)),
                (vec![Action::Cycle(Arrow::Next)], true),
                "press {press}"
            );
            assert_eq!(m.tick(at(t, 300 + press * 1_500)), None, "still the same dictation");
        }
        assert_eq!(m.key_up(at(t, 20_000)), Some(Action::StopHold), "release pastes in the last language");
    }

    #[test]
    fn only_the_two_single_keys_are_key_gestures() {
        assert!(is_key_gesture(FN_KEY));
        assert!(is_key_gesture(RIGHT_OPTION_KEY));
        assert!(!is_key_gesture("CmdOrCtrl+Shift+Space"));
    }
}
