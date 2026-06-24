//! Phase 5L — single-colour status-LED state machine (DFRobot Beetle
//! ESP32-C6 / DFR1117).
//!
//! Same FSM and animation timing as the WS2812 carriers (devkitc,
//! xiao), but rendered on the board's plain on-board LED via LEDC PWM:
//! the colour channel is dropped and each state's *brightness envelope*
//! (pulse / heartbeat / strobe / tick / solid) drives the single LED.
//! So the rig reads "the same way" at a glance — pattern + rhythm carry
//! the state, colour is simply absent. Stays in phase with the other
//! carriers via the synchronised clock, exactly like the WS2812 path.
//!
//! The DFR1117 has no addressable RGB LED — only LED1 (GPIO15, plain,
//! transistor-driven, active-high) and LED4 (LiPo charge status, driven
//! by the TP4057 charger, not software-controllable). See
//! `docs/datasheets/DFR1117-*` + `reference_dfr1117_carrier` memory.
//!
//! NOTE: this duplicates the FSM + `render()` of the WS2812 `led.rs`
//! across carriers. The shared state→signal core wants extracting into
//! a plugin (the LED-plugin direction); until then, this file is the
//! single-colour *backend* and `render()` below is kept byte-identical
//! to the WS2812 carriers so the timing never drifts.

use anyhow::{Context, Result};
use esp_idf_svc::hal::gpio::OutputPin;
use esp_idf_svc::hal::ledc::{
    config::TimerConfig, LedcChannel, LedcDriver, LedcTimer, LedcTimerDriver, Resolution,
};
use esp_idf_svc::hal::peripheral::Peripheral;
use esp_idf_svc::hal::units::FromValueType;
use smart_leds::RGB8;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// FSM state values — wire-compatible with the dashboard's `ledClassFor()`
/// switch in `webapp/index.html`. Identical to the WS2812 carriers.
#[repr(u8)]
#[derive(Clone, Copy, Debug)]
pub enum LedState {
    Boot = 0,             // white flash
    Advertising = 1,      // blue, 1 Hz pulse
    BleConnected = 2,     // cyan, fast pulse
    WifiConnecting = 3,   // cyan→yellow flicker (rendered as BleConnected for now)
    StreamingLive = 4,    // green, heartbeat 60 bpm
    StreamingCatchup = 5, // yellow, heartbeat
    Calibrating = 6,      // purple, solid
    LowBattery = 7,       // orange, slow pulse — overlay via `set_low_battery()`
    Ota = 8,              // white, fast strobe
    Error = 9,            // red, fast pulse
    /// Streaming with synthetic data — ADXL355 init failed; samples
    /// come from the simulator. Rhythmically distinct from `Calibrating`
    /// (pulse vs solid). See `SPEC-R2-WORKSHOP-SENSOR-HEALTH` §4.
    StreamingDegradedSim = 10, // purple, slow pulse 0.5 Hz
    /// Actively recording into a named capture file. A crisp tick every
    /// 0.5 s — distinct from `StreamingLive`'s background heartbeat.
    Recording = 11,
    /// Operator "identify" overlay — solid. Driven by the `identify`
    /// AtomicBool in LedHandle; see `set_identify`.
    Identify = 12,
    /// Graceful low-battery shutdown reached: capture closed, ring
    /// flushed, SD unmounted. On the single-colour LED this renders as a
    /// solid dim glow so the operator can see at a glance the device has
    /// parked itself safely and is about to deep-sleep — distinct from
    /// `Error` (fast pulse). See `Sender::graceful_shutdown` +
    /// SPEC-R2-WORKSHOP-SENSOR §8.4.
    ShuttingDown = 13,
    /// A run has stopped: the SD is being power-safed and the dashboard
    /// is still pulling the run's file to the PC. On the single-colour LED
    /// this renders as a steady mid-brightness "working" double-tick =
    /// "working, do NOT power off yet". Clears to `SafeToPowerOff` once the
    /// file has been served (data_tcp GET complete). See `Sender`
    /// capture-stop handling.
    SecuringData = 14,
    /// Run complete AND its data has reached the PC (data_tcp served the
    /// file). LED off = "safe to power off". The operator's go-signal to
    /// flip the switch. A new run returns the LED to `Recording`.
    SafeToPowerOff = 15,
}

impl LedState {
    fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Boot,
            1 => Self::Advertising,
            2 => Self::BleConnected,
            3 => Self::WifiConnecting,
            4 => Self::StreamingLive,
            5 => Self::StreamingCatchup,
            6 => Self::Calibrating,
            7 => Self::LowBattery,
            8 => Self::Ota,
            9 => Self::Error,
            10 => Self::StreamingDegradedSim,
            11 => Self::Recording,
            12 => Self::Identify,
            13 => Self::ShuttingDown,
            14 => Self::SecuringData,
            15 => Self::SafeToPowerOff,
            _ => Self::Boot,
        }
    }
}

/// Cheap clonable handle used by the rest of the firmware to push state
/// changes to the LED thread. Reads are lock-free atomics.
#[derive(Clone)]
pub struct LedHandle {
    state: Arc<AtomicU8>,
    low_battery: Arc<AtomicBool>,
    ota: Arc<AtomicBool>,
    /// Operator "identify" overlay — highest priority over every other
    /// state. Set / cleared from the streaming-TCP `r2.dash.identify_set`
    /// command (see sender::dispatch_inbound).
    identify: Arc<AtomicBool>,
    /// Auto-revert deadline for the identify overlay, per
    /// `SPEC-R2-WORKSHOP-SENSOR-IDENTIFY` §3.2. If the dashboard fails to
    /// send `identify_set off` within 60 s, the LED loop clears the
    /// overlay so a forgotten Identify doesn't pin the LED on.
    identify_deadline: Arc<Mutex<Option<Instant>>>,
    /// Optional synchronised wall-clock source. When set, the LED loop
    /// computes pulse phases from `clock.ts_ms_i64()` instead of local
    /// elapsed time — so every sensor converged on the dashboard's
    /// time-sync offset pulses / heartbeats / ticks in lockstep,
    /// including across carriers (WS2812 and single-colour alike).
    sync_clock: Arc<Mutex<Option<Arc<crate::clock::Clock>>>>,
}

impl LedHandle {
    pub fn set(&self, state: LedState) {
        self.state.store(state as u8, Ordering::Relaxed);
    }
    pub fn current(&self) -> LedState {
        if self.ota.load(Ordering::Relaxed) {
            return LedState::Ota;
        }
        LedState::from_u8(self.state.load(Ordering::Relaxed))
    }
    /// Low-battery overrides the underlying state while set (slow pulse).
    pub fn set_low_battery(&self, on: bool) {
        self.low_battery.store(on, Ordering::Relaxed);
    }
    /// OTA overlay — fast strobe while receiving + writing an image.
    pub fn set_ota(&self, on: bool) {
        self.ota.store(on, Ordering::Relaxed);
    }
    /// Identify overlay — solid, highest priority, so the operator can
    /// pick this sensor out of a fleet at a glance.
    pub fn set_identify(&self, on: bool) {
        self.identify.store(on, Ordering::Relaxed);
        if let Ok(mut d) = self.identify_deadline.lock() {
            *d = if on { Some(Instant::now() + Duration::from_secs(60)) } else { None };
        }
    }
    /// Plumb the synchronised clock in once it's loaded.
    pub fn attach_clock(&self, clock: Arc<crate::clock::Clock>) {
        if let Ok(mut slot) = self.sync_clock.lock() {
            *slot = Some(clock);
        }
    }
}

/// Spawn the LED animator thread. Returns a handle the rest of the
/// firmware uses to push state changes; the thread runs forever.
///
/// Single-colour LEDC-PWM variant: `timer` + `channel` are an LEDC
/// timer/channel (e.g. `peripherals.ledc.timer0` / `.channel0`) and
/// `gpio` is the on-board LED pin (DFR1117: GPIO15). Brightness is
/// PWM'd so smooth pulses render as breathing, not just on/off blinks.
pub fn start<T, C, P>(timer: T, channel: C, gpio: P) -> Result<LedHandle>
where
    T: Peripheral + Send + 'static,
    <T as Peripheral>::P: LedcTimer,
    C: Peripheral + Send + 'static,
    // Channel and timer must share a SpeedMode (the C6 has only the
    // low-speed group); tie them so LedcDriver::new resolves.
    <C as Peripheral>::P: LedcChannel<SpeedMode = <<T as Peripheral>::P as LedcTimer>::SpeedMode>,
    P: Peripheral + Send + 'static,
    <P as Peripheral>::P: OutputPin,
{
    let state = Arc::new(AtomicU8::new(LedState::Boot as u8));
    let low_battery = Arc::new(AtomicBool::new(false));
    let ota = Arc::new(AtomicBool::new(false));
    let identify = Arc::new(AtomicBool::new(false));
    let identify_deadline: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None));
    let sync_clock: Arc<Mutex<Option<Arc<crate::clock::Clock>>>> = Arc::new(Mutex::new(None));

    let state_for_thread = state.clone();
    let low_for_thread = low_battery.clone();
    let ota_for_thread = ota.clone();
    let identify_for_thread = identify.clone();
    let identify_deadline_for_thread = identify_deadline.clone();
    let clock_for_thread = sync_clock.clone();

    std::thread::Builder::new()
        .stack_size(4096)
        .name("led".into())
        .spawn(move || {
            // Build the LEDC timer + channel inside the thread so the
            // channel's borrow of the timer lives for the thread's
            // (forever) lifetime. 5 kHz, 10-bit duty — flicker-free.
            let timer_cfg = TimerConfig::default()
                .frequency(5.kHz().into())
                .resolution(Resolution::Bits10);
            let timer_drv = match LedcTimerDriver::new(timer, &timer_cfg) {
                Ok(t) => t,
                Err(e) => {
                    log::error!("[LED] LEDC timer init failed: {e}");
                    return;
                }
            };
            let mut drv = match LedcDriver::new(channel, &timer_drv, gpio) {
                Ok(d) => d,
                Err(e) => {
                    log::error!("[LED] LEDC channel init failed: {e}");
                    return;
                }
            };
            run_led_loop(
                &mut drv,
                state_for_thread,
                low_for_thread,
                ota_for_thread,
                identify_for_thread,
                identify_deadline_for_thread,
                clock_for_thread,
            );
        })
        .context("spawn LED thread")?;

    Ok(LedHandle { state, low_battery, ota, identify, identify_deadline, sync_clock })
}

const FRAME_MS: u64 = 33; // ~30 Hz tick — smooth pulses at low CPU cost
// Slow, calm "idle/waiting" beat — 25 BPM gives a 2.4 s lub-dub cycle.
const HEARTBEAT_BPM: f32 = 25.0;
/// Global brightness cap applied after `render()`. Drop below 1.0 if the
/// on-board LED is uncomfortably bright on a bare board.
const BRIGHTNESS: f32 = 1.00;

fn run_led_loop(
    led: &mut LedcDriver<'_>,
    state: Arc<AtomicU8>,
    low_battery: Arc<AtomicBool>,
    ota: Arc<AtomicBool>,
    identify: Arc<AtomicBool>,
    identify_deadline: Arc<Mutex<Option<Instant>>>,
    sync_clock: Arc<Mutex<Option<Arc<crate::clock::Clock>>>>,
) {
    let max_duty = led.get_max_duty();
    let start = Instant::now();
    loop {
        // Identify auto-revert (SPEC-R2-WORKSHOP-SENSOR-IDENTIFY §3.2).
        if let Ok(mut d) = identify_deadline.lock() {
            if let Some(t) = *d {
                if Instant::now() >= t {
                    identify.store(false, Ordering::Relaxed);
                    *d = None;
                }
            }
        }
        let id = identify.load(Ordering::Relaxed);
        let s = if id {
            LedState::Identify
        } else if ota.load(Ordering::Relaxed) {
            LedState::Ota
        } else {
            LedState::from_u8(state.load(Ordering::Relaxed))
        };
        let lb = low_battery.load(Ordering::Relaxed) && !id;

        // Phase source: prefer the synchronised wall clock once plumbed
        // in, reduced into a 60 s window (f32 precision). All periods we
        // use divide 60 s evenly, so the modulo is invisible at the
        // boundary — and every sensor applies the same modulo to the
        // same epoch_ms, so the rig stays in lockstep.
        const PHASE_WINDOW_MS: i64 = 60_000;
        let elapsed = match sync_clock.lock().ok().and_then(|g| g.clone()) {
            Some(clock) => {
                let ms = clock.ts_ms_i64();
                if ms > 0 {
                    Duration::from_millis(ms.rem_euclid(PHASE_WINDOW_MS) as u64)
                } else {
                    start.elapsed()
                }
            }
            None => start.elapsed(),
        };

        // Same render() as the WS2812 carriers — then collapse the RGB
        // to its intensity envelope (max channel) and PWM the mono LED.
        // Active-high: higher duty = brighter.
        let colour = scale(render(s, lb, elapsed), BRIGHTNESS);
        let intensity = colour.r.max(colour.g).max(colour.b) as u32;
        let duty = intensity * max_duty / 255;
        if let Err(e) = led.set_duty(duty) {
            log::warn!("[LED] set_duty failed: {e}");
        }
        std::thread::sleep(Duration::from_millis(FRAME_MS));
    }
}

// ─── Shared pattern logic — kept byte-identical to the WS2812 led.rs so
//     the single-colour LED flashes with exactly the same timing. ──────

/// Map `(state, low_battery, elapsed)` → an RGB8 colour for this frame.
/// All animation maths lives here; the IO loop above is dumb. The C6
/// uses only the intensity of this value (colour is dropped at output).
fn render(state: LedState, low_battery: bool, elapsed: Duration) -> RGB8 {
    if low_battery {
        return scale(rgb(0xFF, 0x80, 0x00), pulse(elapsed, 1.5));
    }

    match state {
        LedState::Boot => {
            if elapsed < Duration::from_millis(100) {
                rgb(0x40, 0x40, 0x40)
            } else {
                rgb(0, 0, 0)
            }
        }
        LedState::Advertising => scale(rgb(0x00, 0x00, 0xFF), pulse(elapsed, 1.0)),
        LedState::BleConnected => scale(rgb(0x00, 0xC0, 0xC0), pulse(elapsed, 0.4)),
        LedState::WifiConnecting => scale(rgb(0x00, 0xC0, 0xC0), pulse(elapsed, 0.4)),
        LedState::StreamingLive => scale(rgb(0x00, 0xC0, 0x20), heartbeat(elapsed, HEARTBEAT_BPM)),
        LedState::StreamingCatchup => scale(rgb(0xFF, 0xCC, 0x00), heartbeat(elapsed, HEARTBEAT_BPM)),
        LedState::Calibrating => rgb(0x80, 0x00, 0xC0),
        LedState::LowBattery => scale(rgb(0xFF, 0x80, 0x00), pulse(elapsed, 1.5)),
        LedState::Ota => strobe(rgb(0x40, 0x40, 0x40), elapsed, 0.18),
        LedState::Error => scale(rgb(0xFF, 0x00, 0x00), pulse(elapsed, 0.25)),
        LedState::StreamingDegradedSim => scale(rgb(0x80, 0x00, 0xC0), pulse(elapsed, 2.0)),
        LedState::Recording => scale(rgb(0x00, 0xE0, 0x30), single_tick(elapsed, 0.5)),
        LedState::Identify => rgb(0xFF, 0xFF, 0xFF),
        // Solid dim glow — graceful low-battery shutdown done (card
        // flushed + unmounted), device about to deep-sleep. Kept
        // byte-identical to the WS2812 carriers (dim red); colour is
        // dropped at output, so this reads as a steady dim glow. Solid
        // (not pulsed like Error) so it reads as "parked safely", and dim
        // to sip the last of the LiPo while the operator notices.
        LedState::ShuttingDown => rgb(0x20, 0x00, 0x00),
        // "Working, don't power off yet" — run stopped, SD power-safed,
        // dashboard still pulling the file to the PC. On the WS2812
        // carriers this is solid teal; here the colour is dropped, so a
        // solid mid-glow would be hard to tell apart from `ShuttingDown`.
        // Instead give it a steady mid-brightness double-tick (a busy
        // "heartbeat") so it unmistakably reads as active work in
        // progress rather than a parked state.
        LedState::SecuringData => scale(rgb(0x00, 0x60, 0x60), heartbeat(elapsed, HEARTBEAT_BPM)),
        // Off — run complete + data confirmed on the PC. "Safe to
        // power off." Dark is the deliberate go-signal.
        LedState::SafeToPowerOff => rgb(0x00, 0x00, 0x00),
    }
}

fn rgb(r: u8, g: u8, b: u8) -> RGB8 { RGB8 { r, g, b } }

/// Scale an RGB triple by 0.0..=1.0.
fn scale(c: RGB8, k: f32) -> RGB8 {
    let k = k.clamp(0.0, 1.0);
    RGB8 {
        r: (c.r as f32 * k) as u8,
        g: (c.g as f32 * k) as u8,
        b: (c.b as f32 * k) as u8,
    }
}

/// Smooth sinusoidal pulse 0..=1 with period `period_secs`.
fn pulse(t: Duration, period_secs: f32) -> f32 {
    let phase = t.as_secs_f32() / period_secs;
    let s = (phase * core::f32::consts::TAU).sin();
    0.5 + 0.5 * s
}

/// Two-beat heartbeat: a quick lub-dub each `60 / bpm` seconds.
fn heartbeat(t: Duration, bpm: f32) -> f32 {
    let period = 60.0 / bpm;
    let phase = (t.as_secs_f32() / period).fract();
    let b1 = (-((phase - 0.00) * 14.0).powi(2)).exp();
    let b2 = (-((phase - 0.18) * 14.0).powi(2)).exp() * 0.7;
    (b1 + b2).clamp(0.0, 1.0)
}

/// Square-wave strobe: full vs off, 50 % duty.
fn strobe(c: RGB8, t: Duration, period_secs: f32) -> RGB8 {
    let phase = (t.as_secs_f32() / period_secs).fract();
    if phase < 0.5 { c } else { rgb(0, 0, 0) }
}

/// Single narrow gaussian tick per `period_secs` (~5 % in), dark for the
/// rest. Used by `LedState::Recording`.
fn single_tick(t: Duration, period_secs: f32) -> f32 {
    let phase = (t.as_secs_f32() / period_secs).fract();
    (-((phase - 0.05) * 14.0).powi(2)).exp()
}
