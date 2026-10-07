//! Hold-to-pick state machine.
//!
//! The original letter is forwarded immediately. Hyprland repeats a key that
//! stays down, and the repeat delay on this machine is 250ms, so the virtual
//! key is released shortly before the picker opens. The physical release is
//! then swallowed and the picker stays up.

use std::collections::HashSet;

pub const KEY_ESC: u16 = 1;
pub const KEY_1: u16 = 2;
pub const KEY_9: u16 = 10;
pub const KEY_BACKSPACE: u16 = 14;
pub const KEY_ENTER: u16 = 28;
pub const KEY_LEFTCTRL: u16 = 29;
pub const KEY_LEFTSHIFT: u16 = 42;
pub const KEY_RIGHTSHIFT: u16 = 54;
pub const KEY_LEFTALT: u16 = 56;
pub const KEY_RIGHTCTRL: u16 = 97;
pub const KEY_RIGHTALT: u16 = 100;
pub const KEY_LEFTMETA: u16 = 125;
pub const KEY_RIGHTMETA: u16 = 126;
pub const KEY_CAPSLOCK: u16 = 58;

const PANIC_GAP_MS: u64 = 1500;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Variant {
    pub text: String,
    pub steps: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct Letter {
    pub code: u16,
    pub variants: Vec<Variant>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    Key { code: u16, value: i32 },
    Show { chars: Vec<String> },
    Hide,
    Replace { steps: Vec<String>, upper: bool },
    Exit,
}

#[derive(Clone, Copy)]
enum Phase {
    Idle,
    Arming {
        key: u16,
        upper: bool,
        started: u64,
        virt_down: bool,
        shown: bool,
    },
}

pub struct Machine {
    letters: Vec<Letter>,
    hold_ms: u64,
    release_ms: u64,
    phase: Phase,
    phys_down: HashSet<u16>,
    swallow_up: HashSet<u16>,
    panic: Vec<(u16, u64)>,
}

impl Machine {
    pub fn new(letters: Vec<Letter>, hold_ms: u64) -> Self {
        let hold_ms = hold_ms.max(2);
        let release_ms = (hold_ms.saturating_mul(220) / 300).clamp(1, hold_ms - 1);
        Self {
            letters,
            hold_ms,
            release_ms,
            phase: Phase::Idle,
            phys_down: HashSet::new(),
            swallow_up: HashSet::new(),
            panic: Vec::new(),
        }
    }

    pub fn ms_until(&self, now: u64) -> i32 {
        match self.phase {
            Phase::Arming {
                started,
                virt_down,
                shown,
                ..
            } => {
                let target = if virt_down {
                    started + self.release_ms
                } else if !shown {
                    started + self.hold_ms
                } else {
                    return 1000;
                };
                target.saturating_sub(now).min(1000) as i32
            }
            Phase::Idle => 1000,
        }
    }

    pub fn held_modifiers(&self) -> Vec<u16> {
        [
            KEY_LEFTSHIFT,
            KEY_RIGHTSHIFT,
            KEY_LEFTCTRL,
            KEY_RIGHTCTRL,
            KEY_LEFTALT,
            KEY_RIGHTALT,
            KEY_LEFTMETA,
            KEY_RIGHTMETA,
        ]
        .into_iter()
        .filter(|code| self.phys_down.contains(code))
        .collect()
    }

    pub fn tick(&mut self, now: u64) -> Vec<Effect> {
        let Phase::Arming {
            key,
            upper,
            started,
            virt_down,
            shown,
        } = self.phase
        else {
            return Vec::new();
        };
        let mut effects = Vec::new();
        let mut virt_down = virt_down;
        let mut shown = shown;
        if virt_down && now.saturating_sub(started) >= self.release_ms {
            effects.push(Effect::Key { code: key, value: 0 });
            virt_down = false;
        }
        if !shown && now.saturating_sub(started) >= self.hold_ms {
            if let Some(letter) = self.letter(key) {
                effects.push(Effect::Show {
                    chars: letter.variants.iter().map(|v| v.text.clone()).collect(),
                });
                shown = true;
            }
        }
        self.phase = Phase::Arming {
            key,
            upper,
            started,
            virt_down,
            shown,
        };
        effects
    }

    pub fn press(&mut self, code: u16, now: u64) -> Vec<Effect> {
        self.phys_down.insert(code);
        if self.note_panic(code, now) {
            self.phase = Phase::Idle;
            return vec![Effect::Hide, Effect::Exit];
        }

        if self.picker_open() {
            return self.press_while_open(code);
        }
        if self.is_arming() {
            return self.press_while_arming(code);
        }
        self.press_while_idle(code, now)
    }

    pub fn release(&mut self, code: u16) -> Vec<Effect> {
        self.phys_down.remove(&code);
        if self.swallow_up.remove(&code) {
            return Vec::new();
        }
        let Phase::Arming {
            key,
            upper,
            started,
            virt_down,
            shown,
        } = self.phase
        else {
            return vec![Effect::Key { code, value: 0 }];
        };
        if code != key {
            return vec![Effect::Key { code, value: 0 }];
        }
        let mut effects = Vec::new();
        if virt_down {
            effects.push(Effect::Key { code, value: 0 });
        }
        if !shown {
            self.phase = Phase::Idle;
        } else {
            self.phase = Phase::Arming {
                key,
                upper,
                started,
                virt_down: false,
                shown: true,
            };
        }
        effects
    }

    fn press_while_open(&mut self, code: u16) -> Vec<Effect> {
        let (key, upper) = match self.phase {
            Phase::Arming { key, upper, .. } => (key, upper),
            Phase::Idle => return vec![Effect::Key { code, value: 1 }],
        };
        if let Some(index) = digit_index(code) {
            if let Some(letter) = self.letter(key) {
                if let Some(variant) = letter.variants.get(index) {
                    let steps = variant.steps.clone();
                    self.swallow_up.insert(code);
                    self.phase = Phase::Idle;
                    return vec![
                        Effect::Hide,
                        Effect::Replace { steps, upper },
                    ];
                }
            }
        }
        if code == KEY_ESC {
            self.swallow_up.insert(code);
            self.phase = Phase::Idle;
            return vec![Effect::Hide];
        }
        self.phase = Phase::Idle;
        vec![Effect::Hide, Effect::Key { code, value: 1 }]
    }

    fn press_while_arming(&mut self, code: u16) -> Vec<Effect> {
        if is_modifier(code) {
            return vec![Effect::Key { code, value: 1 }];
        }
        if let Phase::Arming {
            key, virt_down, ..
        } = self.phase
        {
            if !virt_down {
                self.swallow_up.insert(key);
            }
        }
        self.phase = Phase::Idle;
        vec![Effect::Key { code, value: 1 }]
    }

    fn press_while_idle(&mut self, code: u16, now: u64) -> Vec<Effect> {
        if self.letter(code).is_some() && !self.chord_modifier_down() {
            self.phase = Phase::Arming {
                key: code,
                upper: self.shift_down(),
                started: now,
                virt_down: true,
                shown: false,
            };
        }
        vec![Effect::Key { code, value: 1 }]
    }

    fn picker_open(&self) -> bool {
        matches!(self.phase, Phase::Arming { shown: true, .. })
    }

    fn is_arming(&self) -> bool {
        matches!(self.phase, Phase::Arming { shown: false, .. })
    }

    fn letter(&self, code: u16) -> Option<&Letter> {
        self.letters.iter().find(|letter| letter.code == code)
    }

    fn shift_down(&self) -> bool {
        self.phys_down.contains(&KEY_LEFTSHIFT) || self.phys_down.contains(&KEY_RIGHTSHIFT)
    }

    fn chord_modifier_down(&self) -> bool {
        [KEY_LEFTCTRL, KEY_RIGHTCTRL, KEY_LEFTALT, KEY_RIGHTALT, KEY_LEFTMETA, KEY_RIGHTMETA]
            .into_iter()
            .any(|code| self.phys_down.contains(&code))
    }

    fn note_panic(&mut self, code: u16, now: u64) -> bool {
        if !matches!(code, KEY_BACKSPACE | KEY_ESC | KEY_ENTER) {
            self.panic.clear();
            return false;
        }
        if let Some((_, previous)) = self.panic.last().copied() {
            if now.saturating_sub(previous) > PANIC_GAP_MS {
                self.panic.clear();
            }
        }
        let expected = match self.panic.len() {
            0 => KEY_BACKSPACE,
            1 => KEY_ESC,
            2 => KEY_ENTER,
            _ => {
                self.panic.clear();
                KEY_BACKSPACE
            }
        };
        if code != expected {
            self.panic.clear();
            if code == KEY_BACKSPACE {
                self.panic.push((code, now));
            }
            return false;
        }
        self.panic.push((code, now));
        if self.panic.len() == 3 {
            self.panic.clear();
            return true;
        }
        false
    }
}

fn digit_index(code: u16) -> Option<usize> {
    if (KEY_1..=KEY_9).contains(&code) {
        Some((code - KEY_1) as usize)
    } else {
        None
    }
}

fn is_modifier(code: u16) -> bool {
    matches!(
        code,
        KEY_LEFTSHIFT
            | KEY_RIGHTSHIFT
            | KEY_LEFTCTRL
            | KEY_RIGHTCTRL
            | KEY_LEFTALT
            | KEY_RIGHTALT
            | KEY_LEFTMETA
            | KEY_RIGHTMETA
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY_E: u16 = 18;
    const KEY_X: u16 = 45;

    fn e_letter() -> Letter {
        Letter {
            code: KEY_E,
            variants: ["è", "é", "ê", "ë", "ē", "ė", "ę"]
                .into_iter()
                .map(|text| Variant {
                    text: text.to_string(),
                    steps: vec!["step".into(), "e".into()],
                })
                .collect(),
        }
    }

    fn machine() -> Machine {
        Machine::new(vec![e_letter()], 300)
    }

    fn keys_of(effects: &[Effect]) -> Vec<(u16, i32)> {
        effects
            .iter()
            .filter_map(|effect| match effect {
                Effect::Key { code, value } => Some((*code, *value)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn tap_forwards_the_letter_and_does_not_open() {
        let mut machine = machine();
        let down = machine.press(KEY_E, 0);
        let up = machine.release(KEY_E);
        assert_eq!(keys_of(&down), vec![(KEY_E, 1)]);
        assert_eq!(keys_of(&up), vec![(KEY_E, 0)]);
        assert!(machine.tick(1000).is_empty());
    }

    #[test]
    fn hold_releases_before_repeat_then_shows_e_row() {
        let mut machine = machine();
        machine.press(KEY_E, 0);
        let early = machine.tick(220);
        assert_eq!(keys_of(&early), vec![(KEY_E, 0)]);
        assert!(early.iter().all(|effect| !matches!(effect, Effect::Show { .. })));
        let shown = machine.tick(300);
        match &shown[0] {
            Effect::Show { chars } => {
                assert_eq!(chars[3], "ë");
                assert_eq!(chars.len(), 7);
            }
            other => panic!("expected show, got {other:?}"),
        }
    }

    #[test]
    fn release_after_show_leaves_the_picker_up() {
        let mut machine = machine();
        machine.press(KEY_E, 0);
        machine.tick(220);
        machine.tick(300);
        let up = machine.release(KEY_E);
        assert!(up.is_empty());
        assert!(machine.picker_open());
    }

    #[test]
    fn number_four_selects_e_with_dots() {
        let mut machine = machine();
        machine.press(KEY_E, 0);
        machine.tick(300);
        let picked = machine.press(KEY_1 + 3, 400);
        assert!(picked.iter().any(|effect| matches!(effect, Effect::Hide)));
        match picked.last() {
            Some(Effect::Replace { steps, upper }) => {
                assert_eq!(steps, &vec!["step".to_string(), "e".to_string()]);
                assert!(!*upper);
            }
            other => panic!("expected replace, got {other:?}"),
        }
        assert!(machine.release(KEY_1 + 3).is_empty());
    }

    #[test]
    fn escape_dismisses_without_a_key() {
        let mut machine = machine();
        machine.press(KEY_E, 0);
        machine.tick(300);
        let effects = machine.press(KEY_ESC, 400);
        assert_eq!(effects, vec![Effect::Hide]);
    }

    #[test]
    fn other_key_dismisses_and_is_forwarded() {
        let mut machine = machine();
        machine.press(KEY_E, 0);
        machine.tick(300);
        let effects = machine.press(KEY_X, 400);
        assert!(matches!(effects[0], Effect::Hide));
        assert_eq!(keys_of(&effects), vec![(KEY_X, 1)]);
    }

    #[test]
    fn another_key_before_the_picker_cancels_it() {
        let mut machine = machine();
        machine.press(KEY_E, 0);
        let effects = machine.press(KEY_X, 100);
        assert_eq!(keys_of(&effects), vec![(KEY_X, 1)]);
        assert!(machine.tick(1000).is_empty());
        assert_eq!(keys_of(&machine.release(KEY_E)), vec![(KEY_E, 0)]);
    }

    #[test]
    fn another_key_after_the_early_release_swallows_the_letter_release() {
        let mut machine = machine();
        machine.press(KEY_E, 0);
        machine.tick(220);
        machine.press(KEY_X, 250);
        assert!(machine.release(KEY_E).is_empty());
    }

    #[test]
    fn ctrl_chord_does_not_arm() {
        let mut machine = machine();
        machine.press(KEY_LEFTCTRL, 0);
        machine.press(KEY_E, 10);
        assert!(machine.tick(1000).is_empty());
    }

    #[test]
    fn shifted_letter_selects_the_capital_sequence() {
        let mut machine = machine();
        machine.press(KEY_LEFTSHIFT, 0);
        machine.press(KEY_E, 10);
        machine.tick(310);
        let picked = machine.press(KEY_1 + 3, 400);
        assert!(matches!(
            picked.last(),
            Some(Effect::Replace { upper: true, .. })
        ));
    }

    #[test]
    fn backspace_escape_enter_exits() {
        let mut machine = machine();
        assert!(machine.press(KEY_BACKSPACE, 0).iter().all(|e| !matches!(e, Effect::Exit)));
        assert!(machine.press(KEY_ESC, 200).iter().all(|e| !matches!(e, Effect::Exit)));
        assert!(machine.press(KEY_ENTER, 400).iter().any(|e| matches!(e, Effect::Exit)));
    }
}
