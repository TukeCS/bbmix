//! The mixer state bbmix keeps (and saves), and the maths between it and the card.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Hardware inputs, in the order the Babyface driver names them.
pub const INPUTS: [&str; 12] = [
    "AN1", "AN2", "IN3", "IN4", "AS1", "AS2", "ADAT3", "ADAT4", "ADAT5", "ADAT6", "ADAT7", "ADAT8",
];
/// Hardware outputs (and the software playback channels, which share their names).
pub const OUTPUTS: [&str; 12] = [
    "AN1", "AN2", "PH3", "PH4", "AS1", "AS2", "ADAT3", "ADAT4", "ADAT5", "ADAT6", "ADAT7", "ADAT8",
];
pub const PAIRS: usize = 6;

/// Mixer volumes are linear: 32768 is unity (0 dB), 65536 is +6 dB.
pub const VOL_MAX: u32 = 65536;
pub const VOL_UNITY: u32 = 32768;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bus {
    Input,
    Playback,
}

impl Bus {
    pub fn channel_name(self, ch: usize) -> &'static str {
        match self {
            Bus::Input => INPUTS[ch],
            Bus::Playback => OUTPUTS[ch],
        }
    }
    /// Driver control-name prefix: "Mic-AN1", "Line-IN3", "PCM-AN1" …
    pub fn control_prefix(self, ch: usize) -> String {
        match self {
            Bus::Input if ch < 2 => format!("Mic-{}", INPUTS[ch]),
            Bus::Input => format!("Line-{}", INPUTS[ch]),
            Bus::Playback => format!("PCM-{}", OUTPUTS[ch]),
        }
    }
}

pub fn pair_name(p: usize) -> String {
    let (l, r) = (OUTPUTS[2 * p], OUTPUTS[2 * p + 1]);
    let tag = l.trim_end_matches(|c: char| c.is_ascii_digit());
    let n = |s: &str| s.trim_start_matches(|c: char| c.is_ascii_alphabetic()).to_string();
    format!("{tag} {}/{}", n(l), n(r))
}

/// One channel's send to one output pair.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Send {
    /// level of the louder side, 0..=VOL_MAX
    pub vol: u32,
    /// -1 = hard left, 0 = centre, 1 = hard right
    pub pan: f32,
    #[serde(default)]
    pub mute: bool,
}

impl Default for Send {
    fn default() -> Self {
        Send { vol: 0, pan: 0.0, mute: false }
    }
}

impl Send {
    /// (left, right) raw values as written to the card. Panning turns the far side down,
    /// the same balance law bbfpromix used, so settings read back the same way.
    pub fn lr(&self) -> (i64, i64) {
        if self.mute {
            return (0, 0);
        }
        let v = self.vol.min(VOL_MAX) as f32;
        let p = self.pan.clamp(-1.0, 1.0);
        let (l, r) = if p < 0.0 { (v, v * (1.0 + p)) } else { (v * (1.0 - p), v) };
        (l.round() as i64, r.round() as i64)
    }

    /// The inverse of [`Send::lr`], for values changed outside bbmix.
    pub fn from_lr(l: i64, r: i64) -> Send {
        let (l, r) = (l.max(0) as f32, r.max(0) as f32);
        let vol = l.max(r);
        let pan = if vol == 0.0 || l == r {
            0.0
        } else if l > r {
            -(1.0 - r / l)
        } else {
            1.0 - l / r
        };
        Send { vol: vol.round() as u32, pan, mute: false }
    }
}

/// Fader taper: position 0..1 ↔ linear volume. A 4th-power curve puts 0 dB at ~84 % of
/// travel, −18 dB at half way and gives the quiet end room, like a real console fader.
pub fn fader_to_vol(pos: f32) -> u32 {
    (VOL_MAX as f32 * pos.clamp(0.0, 1.0).powi(4)).round() as u32
}
pub fn vol_to_fader(vol: u32) -> f32 {
    (vol.min(VOL_MAX) as f32 / VOL_MAX as f32).powf(0.25)
}
pub fn vol_db(vol: u32) -> f32 {
    if vol == 0 { f32::NEG_INFINITY } else { 20.0 * (vol as f32 / VOL_UNITY as f32).log10() }
}
pub fn db_to_vol(db: f32) -> u32 {
    ((VOL_UNITY as f32) * 10f32.powf(db / 20.0)).round().min(VOL_MAX as f32) as u32
}
pub fn fmt_db(db: f32) -> String {
    if db == f32::NEG_INFINITY || db < -99.0 {
        "-inf".into()
    } else if db.abs() < 0.05 {
        "0.0".into()
    } else {
        format!("{db:+.1}")
    }
}
pub fn fmt_pan(pan: f32) -> String {
    let p = (pan * 100.0).round() as i32;
    match p {
        0 => "C".into(),
        p if p < 0 => format!("L{}", -p),
        p => format!("R{p}"),
    }
}

/// Volume of one hardware output pair ("Main-Out AN1" + "Main-Out AN2").
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct OutLevel {
    /// 0..=VOL_MAX, scaled onto the control's real range when written
    pub vol: u32,
    #[serde(default)]
    pub mute: bool,
}

impl Default for OutLevel {
    fn default() -> Self {
        OutLevel { vol: VOL_UNITY, mute: false }
    }
}

/// Starting output levels: the driver can't tell us the real ones (it reports
/// silence after every plug-in), so begin at something sensible rather than full blast:
/// main outs −10 dB, phones −20 dB, digital outs 0 dB.
pub fn default_outs() -> Vec<OutLevel> {
    (0..PAIRS)
        .map(|p| {
            let db = match p {
                0 => -10.0,
                1 => -20.0,
                _ => 0.0,
            };
            OutLevel { vol: db_to_vol(db), mute: false }
        })
        .collect()
}

/// Mic / instrument preamp: AN1, AN2 (mic) and IN3, IN4 (instrument / line).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub struct Preamp {
    /// raw control value of "… Gain"
    pub gain: i64,
    #[serde(default)]
    pub phantom: bool,
    #[serde(default)]
    pub pad: bool,
    /// instrument inputs only: index into "Line-INx Sens." (0 = −10 dBV, 1 = +4 dBu)
    #[serde(default)]
    pub sens: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    /// which output pair the faders are showing
    pub output: usize,
    /// [channel][pair]
    pub inputs: Vec<Vec<Send>>,
    pub playback: Vec<Vec<Send>>,
    /// AN1, AN2, IN3, IN4
    pub preamps: Vec<Preamp>,
    /// "Sample Clock Source" item
    pub clock: i64,
    pub spdif: bool,
    pub spdif_emph: bool,
    pub spdif_pro: bool,
    /// your names for channels, keyed "in:AN1" / "pb:AN1"
    pub labels: std::collections::BTreeMap<String, String>,
    /// channel pairs (AN1+AN2, IN3+IN4 …) linked into one stereo strip. Inputs start
    /// mono, playback starts stereo, like TotalMix.
    pub link_in: Vec<bool>,
    pub link_pb: Vec<bool>,
    /// panning turned on per channel; off = centred, heard in both ears
    pub pan_in: Vec<bool>,
    pub pan_pb: Vec<bool>,
    /// hardware output volumes per pair
    pub outs: Vec<OutLevel>,
}

impl Default for State {
    fn default() -> Self {
        // Out of the box each stereo playback pair goes to its own outputs, so audio works.
        let mut playback = vec![vec![Send::default(); PAIRS]; 12];
        for (ch, sends) in playback.iter_mut().enumerate() {
            sends[ch / 2] = Send { vol: VOL_UNITY, pan: 0.0, mute: false };
        }
        State {
            output: 0,
            inputs: vec![vec![Send::default(); PAIRS]; 12],
            playback,
            preamps: vec![Preamp::default(); 4],
            clock: 0,
            spdif: false,
            spdif_emph: false,
            spdif_pro: false,
            labels: Default::default(),
            link_in: vec![false; PAIRS],
            link_pb: vec![true; PAIRS],
            pan_in: vec![false; 12],
            pan_pb: vec![false; 12],
            outs: default_outs(),
        }
    }
}

impl State {
    /// Repairs a hand-edited or old file so every index the UI uses exists.
    fn fixup(mut self) -> State {
        let d = State::default();
        for (v, dv) in [(&mut self.inputs, &d.inputs), (&mut self.playback, &d.playback)] {
            v.resize(12, vec![Send::default(); PAIRS]);
            for (sends, dsends) in v.iter_mut().zip(dv) {
                if sends.len() != PAIRS {
                    *sends = dsends.clone();
                }
            }
        }
        self.preamps.resize(4, Preamp::default());
        self.link_in.resize(PAIRS, false);
        self.link_pb.resize(PAIRS, true);
        self.pan_in.resize(12, false);
        self.pan_pb.resize(12, false);
        // Early builds copied the driver's after-plug-in value (1 = silence) into every
        // output; nobody chooses that on purpose, so treat it as never set.
        if self.outs.len() != PAIRS || self.outs.iter().all(|o| o.vol <= 1) {
            self.outs = default_outs();
        }
        self.output = self.output.min(PAIRS - 1);
        self
    }

    pub fn sends(&self, bus: Bus) -> &Vec<Vec<Send>> {
        match bus {
            Bus::Input => &self.inputs,
            Bus::Playback => &self.playback,
        }
    }
    pub fn sends_mut(&mut self, bus: Bus) -> &mut Vec<Vec<Send>> {
        match bus {
            Bus::Input => &mut self.inputs,
            Bus::Playback => &mut self.playback,
        }
    }

    pub fn linked(&self, bus: Bus, ch: usize) -> bool {
        match bus {
            Bus::Input => self.link_in[ch / 2],
            Bus::Playback => self.link_pb[ch / 2],
        }
    }
    pub fn pan_on(&self, bus: Bus, ch: usize) -> bool {
        match bus {
            Bus::Input => self.pan_in[ch],
            Bus::Playback => self.pan_pb[ch],
        }
    }
    pub fn pan_on_mut(&mut self, bus: Bus, ch: usize) -> &mut bool {
        match bus {
            Bus::Input => &mut self.pan_in[ch],
            Bus::Playback => &mut self.pan_pb[ch],
        }
    }

    /// Links or unlinks a channel pair. Linking keeps the louder fader; unlinking gives
    /// both channels that level, centred.
    pub fn set_linked(&mut self, bus: Bus, ch: usize, on: bool) {
        let first = ch & !1;
        for pair in 0..PAIRS {
            let sends = self.sends_mut(bus);
            let (a, b) = (sends[first][pair], sends[first + 1][pair]);
            let vol = a.vol.max(b.vol);
            let mute = a.mute && b.mute;
            for c in [first, first + 1] {
                sends[c][pair] = Send { vol, pan: 0.0, mute };
            }
        }
        for c in [first, first + 1] {
            *self.pan_on_mut(bus, c) = false;
        }
        match bus {
            Bus::Input => self.link_in[ch / 2] = on,
            Bus::Playback => self.link_pb[ch / 2] = on,
        }
    }

    /// What channel `ch` really sends to output `pair`, as (left, right) raw values.
    /// Mono channels sit in the centre (both ears at full level) unless panning is on.
    /// A linked stereo pair is driven by its first channel's send: left channel → left
    /// output, right channel → right output, and its pan works as a balance.
    pub fn effective_lr(&self, bus: Bus, ch: usize, pair: usize) -> (i64, i64) {
        if self.linked(bus, ch) {
            let first = ch & !1;
            let s = self.sends(bus)[first][pair];
            if s.mute {
                return (0, 0);
            }
            let v = s.vol.min(VOL_MAX) as f32;
            let b = if self.pan_on(bus, first) { s.pan.clamp(-1.0, 1.0) } else { 0.0 };
            if ch == first {
                ((v * (1.0 - b.max(0.0))).round() as i64, 0)
            } else {
                (0, (v * (1.0 + b.min(0.0))).round() as i64)
            }
        } else {
            let mut s = self.sends(bus)[ch][pair];
            if !self.pan_on(bus, ch) {
                s.pan = 0.0;
            }
            s.lr()
        }
    }

    pub fn out(&self, pair: usize) -> OutLevel {
        self.outs.get(pair).copied().unwrap_or_default()
    }
    pub fn out_mut(&mut self, pair: usize) -> &mut OutLevel {
        // Early builds copied the driver's after-plug-in value (1 = silence) into every
        // output; nobody chooses that on purpose, so treat it as never set.
        if self.outs.len() != PAIRS || self.outs.iter().all(|o| o.vol <= 1) {
            self.outs = default_outs();
        }
        &mut self.outs[pair]
    }

    pub fn label_key(bus: Bus, ch: usize) -> String {
        let p = if bus == Bus::Input { "in" } else { "pb" };
        format!("{p}:{}", bus.channel_name(ch))
    }

    pub fn path() -> PathBuf {
        if let Some(p) = std::env::var_os("BBMIX_STATE") {
            return p.into();
        }
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .unwrap_or_else(|| PathBuf::from("."));
        base.join("bbmix").join("state.json")
    }

    /// `None` if there's no saved state yet (first run).
    pub fn load() -> Option<Result<State, String>> {
        let path = Self::path();
        let text = std::fs::read_to_string(&path).ok()?;
        Some(
            serde_json::from_str::<State>(&text)
                .map(State::fixup)
                .map_err(|e| format!("{}: {e}", path.display())),
        )
    }

    /// Atomic save: write a temp file, then rename over the old one.
    pub fn save(&self) -> std::io::Result<()> {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self)?)?;
        std::fs::rename(&tmp, &path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pan_round_trips_through_left_right() {
        for pan in [-1.0, -0.5, -0.25, 0.0, 0.3, 1.0] {
            let s = Send { vol: 30000, pan, mute: false };
            let (l, r) = s.lr();
            let back = Send::from_lr(l, r);
            assert_eq!(back.lr(), (l, r), "pan {pan}");
            assert!((back.pan - pan).abs() < 0.001, "pan {pan} came back as {}", back.pan);
        }
    }

    #[test]
    fn mute_writes_silence_but_keeps_the_level() {
        let s = Send { vol: VOL_UNITY, pan: 0.0, mute: true };
        assert_eq!(s.lr(), (0, 0));
        assert_eq!(s.vol, VOL_UNITY);
    }

    #[test]
    fn fader_taper() {
        assert_eq!(fader_to_vol(0.0), 0);
        assert_eq!(fader_to_vol(1.0), VOL_MAX);
        assert!((vol_db(fader_to_vol(vol_to_fader(VOL_UNITY)))).abs() < 0.01);
        assert!((vol_db(VOL_MAX) - 6.02).abs() < 0.01);
        assert_eq!(db_to_vol(0.0), VOL_UNITY);
    }

    #[test]
    fn pair_names() {
        assert_eq!(pair_name(0), "AN 1/2");
        assert_eq!(pair_name(1), "PH 3/4");
        assert_eq!(pair_name(5), "ADAT 7/8");
    }

    #[test]
    fn outputs_start_at_safe_levels() {
        let s = State::default();
        assert!((vol_db(s.out(0).vol) + 10.0).abs() < 0.01);
        assert!((vol_db(s.out(1).vol) + 20.0).abs() < 0.01);
        // a file saved before outputs existed gets the same defaults
        let old: State = serde_json::from_str(r#"{"outs": []}"#).unwrap();
        assert_eq!(old.fixup().outs, default_outs());
        let silent: State = serde_json::from_str(r#"{"outs": [{"vol":1},{"vol":1},{"vol":1},{"vol":1},{"vol":1},{"vol":1}]}"#).unwrap();
        assert_eq!(silent.fixup().outs, default_outs());
    }

    #[test]
    fn broken_state_file_is_repaired() {
        let s: State = serde_json::from_str(r#"{"output": 9, "inputs": [[]]}"#).unwrap();
        let s = s.fixup();
        assert_eq!(s.output, PAIRS - 1);
        assert_eq!(s.inputs.len(), 12);
        assert!(s.inputs.iter().all(|v| v.len() == PAIRS));
        assert_eq!(s.preamps.len(), 4);
    }

    #[test]
    fn default_routes_playback_to_its_own_output_in_stereo() {
        let s = State::default();
        let u = VOL_UNITY as i64;
        assert_eq!(s.effective_lr(Bus::Playback, 2, 1), (u, 0)); // PH3 → PH3/4 left
        assert_eq!(s.effective_lr(Bus::Playback, 3, 1), (0, u)); // PH4 → PH3/4 right
        assert_eq!(s.effective_lr(Bus::Playback, 2, 0), (0, 0)); // nothing to AN1/2
    }

    #[test]
    fn mics_are_mono_and_centred_until_panning_is_turned_on() {
        let mut s = State::default();
        let u = VOL_UNITY as i64;
        s.inputs[0][0] = Send { vol: VOL_UNITY, pan: -0.5, mute: false };
        assert!(!s.linked(Bus::Input, 0));
        assert_eq!(s.effective_lr(Bus::Input, 0, 0), (u, u)); // both ears
        s.pan_in[0] = true;
        assert_eq!(s.effective_lr(Bus::Input, 0, 0), (u, u / 2));
    }

    #[test]
    fn linking_and_balance() {
        let mut s = State::default();
        let u = VOL_UNITY as i64;
        s.inputs[2][0] = Send { vol: VOL_UNITY / 2, pan: 0.0, mute: false };
        s.inputs[3][0] = Send { vol: VOL_UNITY, pan: 0.0, mute: false };
        s.set_linked(Bus::Input, 3, true);
        assert_eq!(s.effective_lr(Bus::Input, 2, 0), (u, 0)); // louder fader kept
        assert_eq!(s.effective_lr(Bus::Input, 3, 0), (0, u));
        s.pan_in[2] = true;
        s.inputs[2][0].pan = 0.5; // balance right: left channel down
        assert_eq!(s.effective_lr(Bus::Input, 2, 0), (u / 2, 0));
        assert_eq!(s.effective_lr(Bus::Input, 3, 0), (0, u));
        s.set_linked(Bus::Input, 2, false);
        assert_eq!(s.effective_lr(Bus::Input, 3, 0), (u, u)); // back to mono, centred
    }
}
