//! Maps the mixer [`State`] onto the Babyface's ALSA controls, in both directions.

use std::collections::HashMap;

use crate::hw::{Card, Control, Gone};
use crate::model::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    /// (bus, channel, output index 0..12)
    Send(Bus, usize, usize),
    Gain(usize),
    /// hardware output volume, output index 0..12
    Out(usize),
    Phantom(usize),
    Pad(usize),
    Sens(usize),
    Clock,
    Spdif,
    Emph,
    Pro,
}

pub struct Device {
    pub card: Card,
    targets: HashMap<u32, Target>,
}

/// Fallback gain ranges when no card is connected: mic 0–65 dB in 1 dB steps,
/// instrument 0–9 dB in 0.5 dB steps (what the snd-usb-audio Babyface quirk uses).
pub const DEMO_GAIN_MAX: [i64; 4] = [65, 65, 18, 18];

pub fn preamp_control(i: usize) -> String {
    format!("{} Gain", Bus::Input.control_prefix(i))
}

impl Device {
    pub fn connect() -> Option<Device> {
        let card = Card::find_babyface()?;
        let mut d = Device { card, targets: HashMap::new() };
        let add = |d: &mut Device, name: &str, t: Target| {
            if let Some(c) = d.card.control(name) {
                d.targets.insert(c.numid, t);
            }
        };
        for bus in [Bus::Input, Bus::Playback] {
            for ch in 0..12 {
                for (o, out) in OUTPUTS.iter().enumerate() {
                    add(&mut d, &format!("{}-{out}", bus.control_prefix(ch)), Target::Send(bus, ch, o));
                }
            }
        }
        for (o, out) in OUTPUTS.iter().enumerate() {
            add(&mut d, &format!("Main-Out {out}"), Target::Out(o));
        }
        for i in 0..4 {
            add(&mut d, &preamp_control(i), Target::Gain(i));
        }
        for i in 0..2 {
            add(&mut d, &format!("Mic-{} 48V", INPUTS[i]), Target::Phantom(i));
            add(&mut d, &format!("Mic-{} PAD", INPUTS[i]), Target::Pad(i));
            add(&mut d, &format!("Line-{} Sens.", INPUTS[i + 2]), Target::Sens(i + 2));
        }
        add(&mut d, "Sample Clock Source", Target::Clock);
        add(&mut d, "IEC958", Target::Spdif);
        add(&mut d, "IEC958 Emphasis", Target::Emph);
        add(&mut d, "IEC958 Pro Mask", Target::Pro);
        Some(d)
    }

    fn ctl(&self, t: Target) -> Option<&Control> {
        let name = match t {
            Target::Send(bus, ch, o) => format!("{}-{}", bus.control_prefix(ch), OUTPUTS[o]),
            Target::Gain(i) => preamp_control(i),
            Target::Out(o) => format!("Main-Out {}", OUTPUTS[o]),
            Target::Phantom(i) => format!("Mic-{} 48V", INPUTS[i]),
            Target::Pad(i) => format!("Mic-{} PAD", INPUTS[i]),
            Target::Sens(i) => format!("Line-{} Sens.", INPUTS[i]),
            Target::Clock => "Sample Clock Source".into(),
            Target::Spdif => "IEC958".into(),
            Target::Emph => "IEC958 Emphasis".into(),
            Target::Pro => "IEC958 Pro Mask".into(),
        };
        self.card.control(&name)
    }

    fn write(&self, t: Target, v: i64) -> Result<(), Gone> {
        match self.ctl(t) {
            Some(c) => self.card.write(c, v),
            None => Ok(()),
        }
    }
    fn read(&self, t: Target) -> Result<Option<i64>, Gone> {
        match self.ctl(t) {
            Some(c) => self.card.read(c).map(Some),
            None => Ok(None),
        }
    }

    pub fn has_gain(&self) -> bool {
        self.ctl(Target::Gain(0)).is_some()
    }
    /// (min, max) of a preamp's gain control.
    pub fn gain_range(&self, i: usize) -> (i64, i64) {
        self.ctl(Target::Gain(i)).map_or((0, DEMO_GAIN_MAX[i]), |c| (c.min, c.max))
    }
    /// dB for a preamp gain value: the driver's own scale if it has one.
    pub fn gain_db(&self, i: usize, v: i64) -> f32 {
        self.ctl(Target::Gain(i))
            .and_then(|c| self.card.db(c, v))
            .unwrap_or_else(|| gain_db_fallback(i, v, self.gain_range(i).1))
    }
    pub fn clock_items(&self) -> Vec<String> {
        self.ctl(Target::Clock).map(|c| c.items.clone()).unwrap_or_default()
    }

    /// Writes one channel's send to an output pair (both channels if it's a stereo pair).
    pub fn push_send(&self, st: &State, bus: Bus, ch: usize, pair: usize) -> Result<(), Gone> {
        let chans = if st.linked(bus, ch) { vec![ch & !1, (ch & !1) + 1] } else { vec![ch] };
        for c in chans {
            let (l, r) = st.effective_lr(bus, c, pair);
            self.write(Target::Send(bus, c, 2 * pair), l)?;
            self.write(Target::Send(bus, c, 2 * pair + 1), r)?;
        }
        Ok(())
    }

    fn out_range(&self) -> (i64, i64) {
        self.ctl(Target::Out(0)).map_or((0, VOL_MAX as i64), |c| (c.min, c.max))
    }
    /// 0..=VOL_MAX ↔ the control's raw range
    fn out_raw(&self, vol: u32) -> i64 {
        let (min, max) = self.out_range();
        min + (vol.min(VOL_MAX) as i64 * (max - min) + VOL_MAX as i64 / 2) / VOL_MAX as i64
    }
    fn out_norm(&self, raw: i64) -> u32 {
        let (min, max) = self.out_range();
        (((raw - min).clamp(0, max - min) * VOL_MAX as i64 + (max - min) / 2) / (max - min).max(1)) as u32
    }
    /// dB of an output level: the driver's scale if it publishes one.
    pub fn out_db(&self, vol: u32) -> f32 {
        self.ctl(Target::Out(0))
            .and_then(|c| self.card.db(c, self.out_raw(vol)))
            .unwrap_or_else(|| vol_db(vol))
    }

    pub fn push_out(&self, st: &State, pair: usize) -> Result<(), Gone> {
        let o = st.out(pair);
        let raw = if o.mute { self.out_range().0 } else { self.out_raw(o.vol) };
        self.write(Target::Out(2 * pair), raw)?;
        self.write(Target::Out(2 * pair + 1), raw)
    }

    pub fn push_preamp(&self, st: &State, i: usize) -> Result<(), Gone> {
        let p = &st.preamps[i];
        self.write(Target::Gain(i), p.gain)?;
        if i < 2 {
            self.write(Target::Phantom(i), p.phantom as i64)?;
            self.write(Target::Pad(i), p.pad as i64)?;
        } else {
            self.write(Target::Sens(i), p.sens)?;
        }
        Ok(())
    }

    pub fn push_globals(&self, st: &State) -> Result<(), Gone> {
        self.write(Target::Clock, st.clock)?;
        self.write(Target::Spdif, st.spdif as i64)?;
        self.write(Target::Emph, st.spdif_emph as i64)?;
        self.write(Target::Pro, st.spdif_pro as i64)
    }

    /// Restores everything bbmix remembers onto the card.
    pub fn push_all(&self, st: &State) -> Result<(), Gone> {
        for bus in [Bus::Input, Bus::Playback] {
            for ch in 0..12 {
                for p in 0..PAIRS {
                    self.push_send(st, bus, ch, p)?;
                }
            }
        }
        for i in 0..4 {
            self.push_preamp(st, i)?;
        }
        for p in 0..PAIRS {
            self.push_out(st, p)?;
        }
        self.push_globals(st)
    }

    /// Updates `st` from controls that changed on the card. Our own writes echo back as
    /// events too; those match the state already and leave it untouched (so a muted
    /// channel or a pan at zero volume isn't lost). Returns whether anything changed.
    pub fn apply_changes(&self, st: &mut State, numids: &[u32]) -> Result<bool, Gone> {
        let mut changed = false;
        for id in numids {
            let Some(&t) = self.targets.get(id) else { continue };
            let Some(v) = self.read(t)? else { continue };
            let mut set = |dst: &mut i64| {
                if *dst != v {
                    *dst = v;
                    changed = true;
                }
            };
            match t {
                Target::Send(bus, ch, o) => {
                    let pair = o / 2;
                    let other = Target::Send(bus, ch, o ^ 1);
                    let w = self.read(other)?.unwrap_or(0);
                    let (v, w) = (off_is_zero(v), off_is_zero(w));
                    let (l, r) = if o % 2 == 0 { (v, w) } else { (w, v) };
                    if st.effective_lr(bus, ch, pair) != (l, r) {
                        if st.linked(bus, ch) {
                            // a stereo pair: take the new level, keep it stereo
                            let s = &mut st.sends_mut(bus)[ch & !1][pair];
                            s.vol = l.max(r).clamp(0, VOL_MAX as i64) as u32;
                            s.mute = false;
                        } else {
                            let s = Send::from_lr(l, r);
                            if s.pan != 0.0 {
                                *st.pan_on_mut(bus, ch) = true;
                            }
                            st.sends_mut(bus)[ch][pair] = s;
                        }
                        changed = true;
                    }
                }
                Target::Out(o) => {
                    // a pair has one fader: follow the louder side
                    let w = self.read(Target::Out(o ^ 1))?.unwrap_or(v);
                    let v = off_is_zero(v.max(w));
                    let vol = self.out_norm(v);
                    let cur = st.out(o / 2);
                    let shown = if cur.mute { self.out_range().0 } else { self.out_raw(cur.vol) };
                    if shown != v {
                        *st.out_mut(o / 2) = OutLevel { vol, mute: false };
                        changed = true;
                    }
                }
                Target::Gain(i) => set(&mut st.preamps[i].gain),
                Target::Sens(i) => set(&mut st.preamps[i].sens),
                Target::Clock => set(&mut st.clock),
                Target::Phantom(i) => flag(&mut st.preamps[i].phantom, v, &mut changed),
                Target::Pad(i) => flag(&mut st.preamps[i].pad, v, &mut changed),
                Target::Spdif => flag(&mut st.spdif, v, &mut changed),
                Target::Emph => flag(&mut st.spdif_emph, v, &mut changed),
                Target::Pro => flag(&mut st.spdif_pro, v, &mut changed),
            }
        }
        Ok(changed)
    }
}

/// The driver stores a written 0 (off) as 1, so a 1 read back means off.
fn off_is_zero(raw: i64) -> i64 {
    if raw <= 1 { 0 } else { raw }
}

fn flag(dst: &mut bool, v: i64, changed: &mut bool) {
    if *dst != (v != 0) {
        *dst = v != 0;
        *changed = true;
    }
}

/// Mic preamps step in 1 dB, the instrument inputs in 0.5 dB (0–9 dB over 0–18).
pub fn gain_db_fallback(i: usize, v: i64, max: i64) -> f32 {
    if i >= 2 && max == 18 { v as f32 * 0.5 } else { v as f32 }
}
