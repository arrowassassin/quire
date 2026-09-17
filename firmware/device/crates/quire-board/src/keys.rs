//! The seven keys: two ADC resistor ladders (GPIO1: the four keys along the bottom,
//! GPIO2: the key either side of the display) plus the Power GPIO, decoded into
//! `quire_ui::KeyEvent`s with debouncing, long-press, repeat and release timing
//! (brief §2). Pure logic: the HAL feeds samples.

pub use quire_ui::{Key, KeyEvent, KeyKind};

/// Ladder levels in millivolts, measured on an X3 through the ESP32-C3's own ADC.
///
/// The figures in 02-hardware.md §3 came from an X4 and its vendor firmware, and that
/// survey says as much: "shared with X4 ... physical-key→ladder mapping on X3
/// unconfirmed". They are not this device's. They also assume a ladder that idles near
/// 3850 mV, which the C3's ADC cannot report at all: at 11 dB it saturates around 3050,
/// so an idle rail and any key above that ceiling read as the same number. Taken
/// together the old figures put idle 358 mV from Confirm, so the reader sat with Confirm
/// apparently held down for ever — redrawing constantly, and swallowing the keys that
/// were really pressed.
///
/// Measured here, idle is 3052 on both ladders (the ADC ceiling) and every key pulls
/// its ladder down from there.
pub mod levels {
    /// Group 1, the four keys along the bottom, outer left to outer right.
    pub const GROUP1: [(super::Key, u16); 4] =
        [(super::Key::Left, 2620), (super::Key::Back, 1997), (super::Key::Confirm, 1096), (super::Key::Right, 0)];
    /// Group 2, the key either side of the display.
    pub const GROUP2: [(super::Key, u16); 2] = [(super::Key::Up, 1666), (super::Key::Down, 2)];
    /// Above this the ladder is idle (no key). Idle reads 3052; the nearest key is 2620.
    pub const IDLE_ABOVE: u16 = 2850;
    /// How far a sample may sit from a group 1 level and still count as that key.
    ///
    /// The closest pair there is 623 mV apart, so the window has to stay under half of
    /// that or the two keys overlap and the wrong one wins.
    pub const WINDOW1: u16 = 280;
    /// The same for group 2, which has only two levels 1664 mV apart and so can be far
    /// more forgiving. A tight window here is what makes a side key answer only
    /// sometimes: its contact resistance varies with how hard it is pressed, and a
    /// reading that drifts a few hundred millivolts should still be the key it plainly
    /// is, since there is nothing else on that ladder for it to be confused with.
    pub const WINDOW2: u16 = 700;
}

/// Calibrated ladder levels (a unit can store its own from the calibration screen).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ladders {
    /// Group 1 levels.
    pub group1: [(Key, u16); 4],
    /// Group 2 levels.
    pub group2: [(Key, u16); 2],
    /// Idle threshold.
    pub idle_above: u16,
}

impl Default for Ladders {
    fn default() -> Self {
        Ladders { group1: levels::GROUP1, group2: levels::GROUP2, idle_above: levels::IDLE_ABOVE }
    }
}

impl Ladders {
    /// Decode one group's reading: the level nearest the sample, or none when idle or
    /// farther than half a band from every level.
    pub fn decode(levels: &[(Key, u16)], idle_above: u16, mv: u16, window: u16) -> Option<Key> {
        if mv >= idle_above {
            return None;
        }
        let mut best: Option<(Key, u16)> = None;
        for (k, lv) in levels {
            let d = lv.abs_diff(mv);
            if best.is_none_or(|(_, bd)| d < bd) {
                best = Some((*k, d));
            }
        }
        let (k, d) = best?;
        (d <= window).then_some(k)
    }
    /// Decode group 1.
    pub fn group1(&self, mv: u16) -> Option<Key> {
        Self::decode(&self.group1, self.idle_above, mv, levels::WINDOW1)
    }
    /// Decode group 2.
    pub fn group2(&self, mv: u16) -> Option<Key> {
        Self::decode(&self.group2, self.idle_above, mv, levels::WINDOW2)
    }
}

/// Long-press threshold.
pub const LONG_MS: u32 = 500;
/// Repeat period after a long press.
pub const REPEAT_MS: u32 = 200;
/// Samples that must agree before a change is believed (10 ms sampling → 20 ms).
pub const DEBOUNCE_SAMPLES: u8 = 2;

#[derive(Clone, Copy, Debug, Default)]
struct Slot {
    /// Debounced key held on this input.
    held: Option<Key>,
    /// Candidate from the latest samples and how many agreed.
    candidate: Option<Key>,
    agree: u8,
    /// When the held key went down.
    down_at: u32,
    /// Long press already fired.
    long: bool,
    /// Last repeat time.
    last_repeat: u32,
}

/// The key state machine for all three inputs.
#[derive(Clone, Debug, Default)]
pub struct KeyMachine {
    slots: [Slot; 3],
    /// Ladder calibration.
    pub ladders: Ladders,
}

impl KeyMachine {
    /// New with default calibration.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one sample set (group1 mV, group2 mV, power pressed) at `now_ms`; returns up
    /// to three events (one per input) in a small buffer.
    pub fn sample(&mut self, g1_mv: u16, g2_mv: u16, power: bool, now_ms: u32) -> heapless::Vec<KeyEvent, 6> {
        let mut out = heapless::Vec::new();
        let reads = [self.ladders.group1(g1_mv), self.ladders.group2(g2_mv), power.then_some(Key::Power)];
        for (slot, read) in self.slots.iter_mut().zip(reads) {
            if read != slot.candidate {
                slot.candidate = read;
                slot.agree = 1;
            } else if slot.agree < DEBOUNCE_SAMPLES {
                slot.agree += 1;
            }
            let believed = if slot.agree >= DEBOUNCE_SAMPLES { slot.candidate } else { slot.held };
            match (slot.held, believed) {
                (None, Some(k)) => {
                    slot.held = Some(k);
                    slot.down_at = now_ms;
                    slot.long = false;
                }
                (Some(k), None) => {
                    slot.held = None;
                    let _ = out.push(if slot.long { KeyEvent { key: k, kind: KeyKind::Release } } else { KeyEvent::press(k) });
                }
                (Some(k), Some(k2)) if k != k2 => {
                    // Slid from one ladder key to another: release the first, start the second.
                    let _ = out.push(if slot.long { KeyEvent { key: k, kind: KeyKind::Release } } else { KeyEvent::press(k) });
                    slot.held = Some(k2);
                    slot.down_at = now_ms;
                    slot.long = false;
                }
                (Some(k), Some(_)) => {
                    let held_ms = now_ms.wrapping_sub(slot.down_at);
                    if !slot.long && held_ms >= LONG_MS {
                        slot.long = true;
                        slot.last_repeat = now_ms;
                        let _ = out.push(KeyEvent::long(k));
                    } else if slot.long && now_ms.wrapping_sub(slot.last_repeat) >= REPEAT_MS {
                        slot.last_repeat = now_ms;
                        let _ = out.push(KeyEvent { key: k, kind: KeyKind::Repeat });
                    }
                }
                (None, None) => {}
            }
        }
        out
    }

    /// Whether any key is currently held (keeps the device out of light sleep).
    pub fn any_held(&self) -> bool {
        self.slots.iter().any(|s| s.held.is_some())
    }

    /// Whether any input read as pressed in the last sample, before debounce believed it.
    ///
    /// The nap between page turns ends on this rather than on [`KeyMachine::any_held`]:
    /// one sample showing contact is enough to bring the loop back to full speed, so the
    /// two agreeing samples a press needs are taken 10 ms apart as usual and no tap is
    /// ever missed for having started inside a nap.
    pub fn any_touched(&self) -> bool {
        self.slots.iter().any(|s| s.held.is_some() || s.candidate.is_some())
    }

    /// Whether Power is held right now.
    pub fn power_held(&self) -> bool {
        self.slots[2].held == Some(Key::Power)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate std;
    use std::vec::Vec;

    fn run(m: &mut KeyMachine, samples: &[(u16, u16, bool)], step: u32) -> Vec<KeyEvent> {
        let mut out = Vec::new();
        for (i, (a, b, p)) in samples.iter().enumerate() {
            out.extend(m.sample(*a, *b, *p, i as u32 * step));
        }
        out
    }

    #[test]
    fn decodes_ladders() {
        let l = Ladders::default();
        // The readings an X3 actually gives, key by key.
        assert_eq!(l.group1(2620), Some(Key::Left));
        assert_eq!(l.group1(1997), Some(Key::Back));
        assert_eq!(l.group1(1096), Some(Key::Confirm));
        assert_eq!(l.group1(0), Some(Key::Right));
        assert_eq!(l.group2(1666), Some(Key::Up));
        assert_eq!(l.group2(2), Some(Key::Down));
        // Idle is 3052 on both ladders and must read as no key at all: this is the
        // reading that used to come back as Confirm held down for ever.
        assert_eq!(l.group1(3052), None);
        assert_eq!(l.group2(3052), None);
        assert_eq!(l.group1(4095), None);
        // Neighbours are 623 mV apart, so the midpoint between them belongs to neither.
        assert_eq!(l.group1((2620 + 1997) / 2), None);
    }

    #[test]
    fn short_press_needs_debounce() {
        let mut m = KeyMachine::new();
        // One noisy sample does nothing; two agreeing samples press, two idle release.
        let ev = run(&mut m, &[(1096, 4095, false), (4095, 4095, false), (4095, 4095, false)], 10);
        assert!(ev.is_empty());
        let ev = run(&mut m, &[(1096, 4095, false), (1096, 4095, false), (4095, 4095, false), (4095, 4095, false)], 10);
        assert_eq!(ev, [KeyEvent::press(Key::Confirm)]);
    }

    #[test]
    fn long_press_repeats_and_releases() {
        let mut m = KeyMachine::new();
        let mut samples: Vec<(u16, u16, bool)> = std::iter::repeat_n((5, 4095, false), 100).collect(); // 1 s held
        samples.extend([(4095, 4095, false), (4095, 4095, false)]);
        let ev = run(&mut m, &samples, 10);
        assert_eq!(ev[0], KeyEvent::long(Key::Right));
        let repeats = ev.iter().filter(|e| e.kind == KeyKind::Repeat).count();
        assert!((2..=3).contains(&repeats), "{repeats} repeats in 500 ms");
        assert_eq!(*ev.last().unwrap(), KeyEvent { key: Key::Right, kind: KeyKind::Release });
        assert!(!ev.iter().any(|e| e.kind == KeyKind::Press));
    }

    #[test]
    fn a_tap_that_starts_during_a_nap_is_not_missed() {
        // Idle sampling naps in 25 ms slices; the first sample that sees contact reports
        // `any_touched`, which is what returns the loop to 10 ms sampling. The two
        // agreeing samples a press needs are then taken at the usual spacing, so a tap
        // that began inside a nap still presses.
        let mut m = KeyMachine::new();
        let mut out = Vec::new();
        let mut t = 0u32;
        for _ in 0..2 {
            out.extend(m.sample(4095, 4095, false, t));
            assert!(!m.any_touched());
            t += 25;
        }
        out.extend(m.sample(1096, 4095, false, t));
        assert!(m.any_touched(), "first contact has to end the nap");
        for _ in 0..4 {
            t += 10;
            out.extend(m.sample(1096, 4095, false, t));
        }
        for _ in 0..2 {
            t += 10;
            out.extend(m.sample(4095, 4095, false, t));
        }
        assert_eq!(out, [KeyEvent::press(Key::Confirm)]);
    }

    #[test]
    fn power_and_side_keys_are_independent() {
        let mut m = KeyMachine::new();
        let ev = run(&mut m, &[(4095, 1666, true), (4095, 1666, true), (4095, 4095, false), (4095, 4095, false)], 10);
        assert!(ev.contains(&KeyEvent::press(Key::Up)));
        assert!(ev.contains(&KeyEvent::press(Key::Power)));
    }
}
