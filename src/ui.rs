//! The window, laid out like RME's TotalMix: a top bar (device, clock, S/PDIF) over
//! three rows — hardware inputs (with preamps), software playback, and hardware
//! outputs. Click an output to mix to it: the two rows above show its submix.
//! Colours and type match weather_shark / tuke.dev.

use std::path::PathBuf;

use egui::{
    Align2, Color32, CornerRadius, FontFamily, FontId, Id, Pos2, Rect, RichText, Sense, Stroke,
    StrokeKind, Ui, pos2, vec2,
};

use crate::device::{DEMO_GAIN_MAX, Device, gain_db_fallback};
use crate::model::*;

// palette (matches weather_shark / tuke.dev)
const BG: Color32 = Color32::from_rgb(0x11, 0x11, 0x13);
const CARD: Color32 = Color32::from_rgb(0x18, 0x18, 0x1b);
const WELL: Color32 = Color32::from_rgb(0x14, 0x14, 0x16);
const LINE: Color32 = Color32::from_rgb(0x26, 0x26, 0x2b);
const FG: Color32 = Color32::from_rgb(0xe7, 0xe7, 0xea);
const MUTED: Color32 = Color32::from_rgb(0x8e, 0x8e, 0x95);
const ACCENT: Color32 = Color32::from_rgb(0xa7, 0x8b, 0xfa);
const RED: Color32 = Color32::from_rgb(0xff, 0x37, 0x5f);
const GREEN: Color32 = Color32::from_rgb(0x30, 0xd1, 0x58);

const BAR_H: f32 = 44.0;
const NAME_H: f32 = 38.0;
const PRE_H: f32 = 98.0;
const PAN_H: f32 = 42.0;
const FOOT_H: f32 = 50.0;
const GAP: f32 = 6.0;
const CARD_PAD: f32 = 10.0;
const MIN_FADER: f32 = 90.0;

/// What changed this frame and has to go to the card.
enum Push {
    Send(Bus, usize, usize),
    Preamp(usize),
    Out(usize),
    Globals,
}

pub struct Opts {
    /// never touch the hardware (try the UI without the interface)
    pub demo: bool,
    pub screenshot: Option<PathBuf>,
}

pub struct App {
    st: State,
    dev: Option<Device>,
    opts: Opts,
    /// a saved state exists, so it's what gets restored on connect
    have_saved: bool,
    dirty_at: Option<f64>,
    next_probe: f64,
    save_error: Option<String>,
    load_error: Option<String>,
    /// renaming a strip: (label key, text so far)
    editing: Option<(String, String)>,
    pushes: Vec<Push>,
    frames: u32,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, opts: Opts) -> Self {
        apply_style(&cc.egui_ctx);
        let (st, have_saved, load_error) = match State::load() {
            Some(Ok(s)) => (s, true, None),
            // a broken file: start fresh but don't overwrite it until something changes
            Some(Err(e)) => (State::default(), false, Some(e)),
            None => (State::default(), false, None),
        };
        App {
            st,
            dev: None,
            opts,
            have_saved,
            dirty_at: None,
            next_probe: 0.0,
            save_error: None,
            load_error,
            editing: None,
            pushes: Vec::new(),
            frames: 0,
        }
    }

    fn dirty(&mut self, now: f64) {
        self.dirty_at.get_or_insert(now);
    }

    /// Connects / reconnects, and follows changes made on the card by other programs.
    fn poll_device(&mut self, now: f64) {
        if self.opts.demo {
            return;
        }
        if self.dev.is_none() {
            if now < self.next_probe {
                return;
            }
            self.next_probe = now + 1.5;
            let Some(dev) = Device::connect() else { return };
            // The driver forgets the mixer when the interface is power-cycled, so what
            // we remember goes back onto the card. First run: adopt the card's settings.
            let res = if self.have_saved {
                dev.push_all(&self.st)
            } else {
                let r = dev.pull_all(&mut self.st);
                self.have_saved = true;
                self.dirty(now);
                r
            };
            // output volumes are only ever taken from the card the first time
            let res = res.and_then(|_| dev.pull_outs_if_unknown(&mut self.st));
            if let Ok(changed) = res {
                if changed {
                    self.dirty(now);
                }
                self.dev = Some(dev);
            }
            return;
        }
        let dev = self.dev.as_ref().unwrap();
        match dev.card.changes().and_then(|ids| dev.apply_changes(&mut self.st, &ids)) {
            Ok(true) => self.dirty(now),
            Ok(false) => {}
            Err(_) => self.dev = None,
        }
    }

    fn flush_pushes(&mut self) {
        let pushes = std::mem::take(&mut self.pushes);
        let Some(dev) = &self.dev else { return };
        for p in pushes {
            let r = match p {
                Push::Send(bus, ch, pair) => dev.push_send(&self.st, bus, ch, pair),
                Push::Preamp(i) => dev.push_preamp(&self.st, i),
                Push::Out(p) => dev.push_out(&self.st, p),
                Push::Globals => dev.push_globals(&self.st),
            };
            if r.is_err() {
                self.dev = None;
                return;
            }
        }
    }

    fn save_if_due(&mut self, now: f64, force: bool) {
        let Some(t) = self.dirty_at else { return };
        if !force && now - t < 0.6 {
            return;
        }
        self.dirty_at = None;
        self.have_saved = true;
        self.load_error = None;
        self.save_error = self.st.save().err().map(|e| e.to_string());
    }

    // --- top bar -------------------------------------------------------------------

    fn top_bar(&mut self, ui: &mut Ui, now: f64) {
        let narrow = ui.available_width() < 1100.0;
        ui.horizontal_centered(|ui| {
            if narrow {
                ui.spacing_mut().item_spacing.x = 5.0;
                ui.spacing_mut().button_padding.x = 6.0;
            }
            let (r, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
            ui.painter().circle_filled(r.center(), 4.0, ACCENT);
            ui.label(RichText::new("bbmix").color(FG));
            ui.separator();

            // device status
            let (r, resp) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
            let (dot, text, hint) = match &self.dev {
                Some(d) => (
                    GREEN,
                    "babyface".to_string(),
                    format!(
                        "{} connected (hw:{}). Your mix is saved as you go and put back on the card every time it connects.",
                        d.card.name,
                        d.card.index
                    ),
                ),
                None if self.opts.demo => (MUTED, "demo".into(), "started with --demo: no hardware is touched".into()),
                None => {
                    let k = (0.5 + 0.5 * (now * 3.0).sin()) as f32;
                    (
                        MUTED.gamma_multiply(0.4 + 0.6 * k),
                        "waiting…".into(),
                        "plug in / power on the interface. You can still set things up: they're sent when it connects.".into(),
                    )
                }
            };
            ui.painter().circle_filled(r.center(), 4.0, dot);
            let col = if self.dev.is_some() { FG } else { MUTED };
            ui.label(RichText::new(text).color(col)).union(resp).on_hover_text(hint);
            ui.separator();

            ui.label(RichText::new("mixing to").color(MUTED));
            ui.label(RichText::new(pair_name(self.st.output)).color(ACCENT))
                .on_hover_text("click an output in the bottom row (or press 1–6) to mix to it");

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // save indicator
                let path = State::path().display().to_string();
                if let Some(e) = &self.save_error {
                    ui.label(RichText::new("save failed").color(RED)).on_hover_text(format!("{path}: {e}"));
                } else if let Some(e) = &self.load_error {
                    ui.label(RichText::new("config unreadable").color(RED))
                        .on_hover_text(format!("{e}\nStarted from defaults; it's replaced when you change something."));
                } else if self.dirty_at.is_some() {
                    ui.label(RichText::new("saving…").color(MUTED));
                } else {
                    let t = "saved";
                    ui.label(RichText::new(t).color(MUTED)).on_hover_text(format!("saved to {path}"));
                }
                ui.separator();
                self.spdif_menu(ui, now);
                self.clock_combo(ui, now, narrow);
            });
        });
    }

    fn clock_combo(&mut self, ui: &mut Ui, now: f64, narrow: bool) {
        let mut items = self.dev.as_ref().map(|d| d.clock_items()).unwrap_or_default();
        if items.is_empty() {
            items = vec!["AutoSync".into(), "Internal".into()];
        }
        let cur = items.get(self.st.clock as usize).cloned().unwrap_or_else(|| "?".into());
        egui::ComboBox::from_id_salt("clock")
            .selected_text(if narrow { cur.to_lowercase() } else { format!("clock {}", cur.to_lowercase()) })
            .width(0.0)
            .show_ui(ui, |ui| {
                for (i, it) in items.iter().enumerate() {
                    if ui.selectable_value(&mut self.st.clock, i as i64, it.to_lowercase()).changed() {
                        self.pushes.push(Push::Globals);
                        self.dirty(now);
                    }
                }
            })
            .response
            .on_hover_text("sample clock source");
    }

    fn spdif_menu(&mut self, ui: &mut Ui, now: f64) {
        let on = self.st.spdif || self.st.spdif_emph || self.st.spdif_pro;
        let btn = ui.button(if on { RichText::new("s/pdif ●").color(ACCENT) } else { RichText::new("s/pdif") });
        egui::Popup::menu(&btn)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .show(|ui| {
                ui.set_min_width(180.0);
                ui.label(RichText::new("DIGITAL I/O").color(MUTED).small());
                let mut changed = false;
                changed |= ui.checkbox(&mut self.st.spdif, "S/PDIF").changed();
                changed |= ui.checkbox(&mut self.st.spdif_emph, "emphasis").changed();
                changed |= ui.checkbox(&mut self.st.spdif_pro, "professional").changed();
                if changed {
                    self.pushes.push(Push::Globals);
                    self.dirty(now);
                }
            });
    }

    // --- mixer ---------------------------------------------------------------------

    /// A rounded card with a small heading; `add` fills the rest.
    fn card_frame(ui: &mut Ui, height: f32, title: &str, sub: &str, hint: &str, add: impl FnOnce(&mut Ui)) {
        egui::Frame::new()
            .fill(CARD)
            .stroke(Stroke::new(1.0, LINE))
            .corner_radius(10)
            .inner_margin(CARD_PAD as i8)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.set_height(height - 2.0 * CARD_PAD);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(title).color(MUTED).small());
                    ui.label(RichText::new(sub).color(ACCENT).small());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new(hint).color(MUTED.gamma_multiply(0.6)).small());
                    });
                });
                ui.add_space(4.0);
                add(ui);
            });
    }

    /// Width of one strip slot and the gap between slots for a card `w` wide.
    fn slots(w: f32) -> (f32, f32) {
        let gap = if w < 1000.0 { 4.0 } else { GAP };
        (((w - gap * 11.0) / 12.0).clamp(54.0, 140.0), gap)
    }

    fn card(&mut self, ui: &mut Ui, bus: Bus, height: f32, now: f64) {
        let title = match bus {
            Bus::Input => "HARDWARE INPUTS",
            Bus::Playback => "SOFTWARE PLAYBACK",
        };
        let sub = format!("→ {}", pair_name(self.st.output));
        let hint = "drag · scroll · double-click resets · shift = fine";
        Self::card_frame(ui, height, title, &sub, hint, |ui| {
                let h = ui.available_height();
                let w = ui.available_width();
                // strips stretch to fill the card; below 54 px each the row scrolls instead
                let (sw, gap) = Self::slots(w);
                let row_w = sw * 12.0 + gap * 11.0;
                let mut draw = |ui: &mut Ui| {
                    let (row, _) = ui.allocate_exact_size(vec2(row_w, h - 4.0), Sense::hover());
                    let slot = |ch: usize, n: f32| {
                        let x = row.left() + ch as f32 * (sw + gap);
                        Rect::from_min_size(pos2(x, row.top()), vec2(sw * n + gap * (n - 1.0), row.height()))
                    };
                    for first in (0..12).step_by(2) {
                        if self.st.linked(bus, first) {
                            self.strip(ui, slot(first, 2.0), bus, first, true, now);
                        } else {
                            self.strip(ui, slot(first, 1.0), bus, first, false, now);
                            self.strip(ui, slot(first + 1, 1.0), bus, first + 1, false, now);
                        }
                    }
                };
                if row_w <= w + 0.5 {
                    draw(ui);
                } else {
                    egui::ScrollArea::horizontal()
                        .id_salt(title)
                        .scroll_source(egui::scroll_area::ScrollSource::SCROLL_BAR)
                        .show(ui, |ui| draw(ui));
                }
        });
    }

    /// The bottom row: one stereo strip per output pair with its volume. Clicking a
    /// strip makes it the output the rows above mix to.
    fn outputs_card(&mut self, ui: &mut Ui, height: f32, now: f64) {
        let hint = "click an output to mix to it";
        Self::card_frame(ui, height, "HARDWARE OUTPUTS", "", hint, |ui| {
            let h = ui.available_height();
            let w = ui.available_width();
            let (sw, gap) = Self::slots(w);
            let row_w = sw * 12.0 + gap * 11.0;
            let mut draw = |ui: &mut Ui| {
                let (row, _) = ui.allocate_exact_size(vec2(row_w, h - 4.0), Sense::hover());
                for p in 0..PAIRS {
                    let x = row.left() + (2 * p) as f32 * (sw + gap);
                    let rect = Rect::from_min_size(pos2(x, row.top()), vec2(sw * 2.0 + gap, row.height()));
                    self.out_strip(ui, rect, p, now);
                }
            };
            if row_w <= w + 0.5 {
                draw(ui);
            } else {
                egui::ScrollArea::horizontal()
                    .id_salt("outs")
                    .scroll_source(egui::scroll_area::ScrollSource::SCROLL_BAR)
                    .show(ui, |ui| draw(ui));
            }
        });
    }

    fn out_strip(&mut self, ui: &mut Ui, rect: Rect, pair: usize, now: f64) {
        let id = Id::NULL.with(("out", pair));
        let selected = self.st.output == pair;
        // the whole strip selects; the fader and buttons drawn after it take their own clicks
        let body = ui.interact(rect, id.with("body"), Sense::click());
        let hovered = ui.rect_contains_pointer(rect);
        let p = ui.painter().clone();
        let fill = if selected {
            ACCENT.gamma_multiply(0.10)
        } else if hovered {
            Color32::from_rgb(0x1c, 0x1c, 0x20)
        } else {
            WELL
        };
        p.rect_filled(rect, 8, fill);
        let stroke = if selected { Stroke::new(1.5, ACCENT) } else { Stroke::new(1.0, LINE) };
        p.rect_stroke(rect, 8, stroke, StrokeKind::Inside);

        let name_r = Rect::from_min_size(rect.min, vec2(rect.width(), NAME_H));
        let key = format!("out:{}", OUTPUTS[2 * pair]);
        let kind = match pair {
            0 => "main",
            1 => "phones",
            2 => "s/pdif",
            _ => "adat",
        };
        let chosen = self.name_box(ui, name_r, key, &pair_name(pair), kind, now);

        let before = self.st.out(pair);
        let mut s = Send { vol: before.vol, pan: 0.0, mute: before.mute };
        let fader_r = Rect::from_min_max(pos2(rect.left(), rect.top() + NAME_H), pos2(rect.right(), rect.bottom() - FOOT_H));
        fader_widget(ui, fader_r, id.with("fader"), &mut s);

        let db = match &self.dev {
            Some(d) => d.out_db(s.vol),
            None => vol_db(s.vol),
        };
        let col = if s.mute || s.vol == 0 { MUTED } else { FG };
        p.text(pos2(rect.center().x, rect.bottom() - FOOT_H + 10.0), Align2::CENTER_CENTER, fmt_db(db), mono(12.0), col);

        let row = Rect::from_center_size(pos2(rect.center().x, rect.bottom() - 18.0), vec2((rect.width() - 12.0).min(130.0), 22.0));
        let w = (row.width() - 4.0) / 2.0;
        let m = Rect::from_min_size(row.min, vec2(w, row.height()));
        let mix_r = Rect::from_min_size(pos2(row.left() + w + 4.0, row.top()), vec2(w, row.height()));
        if pill(ui, m, id.with("mute"), s.mute, "M", RED).on_hover_text("mute this output").clicked() {
            s.mute = !s.mute;
        }
        let mix = pill(ui, mix_r, id.with("mix"), selected, "mix", ACCENT)
            .on_hover_text(format!("mix to {} (key {})", pair_name(pair), pair + 1));

        if body.clicked() || chosen || mix.clicked() {
            self.st.output = pair;
            self.dirty(now);
        }
        let after = OutLevel { vol: s.vol, mute: s.mute };
        if after != before {
            *self.st.out_mut(pair) = after;
            self.pushes.push(Push::Out(pair));
            self.dirty(now);
        }
    }

    /// One channel strip, or (`stereo`) one double-width strip for a linked pair.
    fn strip(&mut self, ui: &mut Ui, rect: Rect, bus: Bus, ch: usize, stereo: bool, now: f64) {
        let id = Id::NULL.with(("strip", bus as u8, ch));
        let pair = self.st.output;
        let hovered = ui.rect_contains_pointer(rect);
        let p = ui.painter().clone();
        p.rect_filled(rect, 8, if hovered { Color32::from_rgb(0x1c, 0x1c, 0x20) } else { WELL });
        p.rect_stroke(rect, 8, Stroke::new(1.0, LINE), StrokeKind::Inside);

        let mut y = rect.top();
        // name: your label (double-click to rename) over the channel id
        let name_r = Rect::from_min_size(pos2(rect.left(), y), vec2(rect.width(), NAME_H));
        let key = State::label_key(bus, ch);
        let chan = if stereo {
            // "AN1/2", "ADAT3/4"
            let b = bus.channel_name(ch + 1);
            let digits = b.trim_start_matches(|c: char| c.is_ascii_alphabetic());
            format!("{}/{digits}", bus.channel_name(ch))
        } else {
            bus.channel_name(ch).to_string()
        };
        let kind = match (bus, ch) {
            (Bus::Playback, _) => "pcm",
            (_, 0 | 1) => "mic",
            (_, 2 | 3) => "inst",
            (_, 4 | 5) => "s/pdif",
            _ => "adat",
        };
        let kind = if stereo { format!("{kind} · stereo") } else { kind.to_string() };
        self.name_box(ui, name_r, key, &chan, &kind, now);
        y += NAME_H;

        if bus == Bus::Input {
            let pre = Rect::from_min_size(pos2(rect.left(), y), vec2(rect.width(), PRE_H));
            if stereo && ch < 4 {
                // a linked mic / instrument pair keeps a preamp per side
                let half = vec2(pre.width() / 2.0, PRE_H);
                self.preamp(ui, Rect::from_min_size(pre.min, half), ch, now);
                self.preamp(ui, Rect::from_min_size(pre.min + vec2(half.x, 0.0), half), ch + 1, now);
            } else if ch < 4 {
                self.preamp(ui, pre, ch, now);
            }
            y += PRE_H;
            divider(&p, rect, y - 4.0);
        }

        let send = self.st.sends(bus)[ch][pair];
        let mut s = send;
        let pan_was = self.st.pan_on(bus, ch);
        let mut pan_on = pan_was;

        // pan: off by default (centred, both ears); turning it on shows the slider.
        // On a stereo pair it works as a balance.
        let word = if stereo { "bal" } else { "pan" };
        let pill_r = Rect::from_center_size(
            pos2(rect.center().x, y + 29.0),
            vec2((rect.width() - 16.0).min(if stereo { 110.0 } else { 70.0 }), 18.0),
        );
        if pan_on {
            let pan_r = Rect::from_min_max(pos2(rect.left() + 10.0, y + 3.0), pos2(rect.right() - 10.0, y + 17.0));
            pan_widget(ui, pan_r, id.with("pan"), &mut s.pan);
        }
        let (text, hint) = if pan_on {
            (fmt_pan(s.pan), format!("{word}: on. Click to turn it off (back to centre)"))
        } else if stereo {
            (word.to_string(), "stereo: left channel → left ear, right → right. Click to add a balance control".to_string())
        } else {
            (word.to_string(), "mono: heard in both ears. Click to pan it".to_string())
        };
        if pill(ui, pill_r, id.with("pan_on"), pan_on, &text, ACCENT).on_hover_text(hint).clicked() {
            pan_on = !pan_on;
            if !pan_on {
                s.pan = 0.0;
            }
        }
        y += PAN_H;

        // fader
        let fader_r = Rect::from_min_max(pos2(rect.left(), y), pos2(rect.right(), rect.bottom() - FOOT_H));
        fader_widget(ui, fader_r, id.with("fader"), &mut s);

        // level readout + mute
        let db = vol_db(s.vol);
        let col = if s.mute || s.vol == 0 { MUTED } else { FG };
        p.text(
            pos2(rect.center().x, rect.bottom() - FOOT_H + 10.0),
            Align2::CENTER_CENTER,
            fmt_db(db),
            mono(12.0),
            col,
        );
        // mute + stereo link, side by side
        let row = Rect::from_center_size(
            pos2(rect.center().x, rect.bottom() - 18.0),
            vec2((rect.width() - 12.0).min(if stereo { 130.0 } else { 80.0 }), 22.0),
        );
        let w = (row.width() - 4.0) / 2.0;
        let m = Rect::from_min_size(row.min, vec2(w, row.height()));
        let st_r = Rect::from_min_size(pos2(row.left() + w + 4.0, row.top()), vec2(w, row.height()));
        if pill(ui, m, id.with("mute"), s.mute, "M", RED).on_hover_text("mute").clicked() {
            s.mute = !s.mute;
        }
        let other = bus.channel_name(ch ^ 1);
        let link_hint = if stereo {
            "stereo pair: click to split into two mono channels".to_string()
        } else {
            format!("mono: click to link with {other} as a stereo pair")
        };
        let link_clicked = pill(ui, st_r, id.with("link"), stereo, "ST", ACCENT).on_hover_text(link_hint).clicked();

        if s != send || pan_on != pan_was {
            self.st.sends_mut(bus)[ch][pair] = s;
            *self.st.pan_on_mut(bus, ch) = pan_on;
            self.pushes.push(Push::Send(bus, ch, pair));
            self.dirty(now);
        }
        if link_clicked {
            self.st.set_linked(bus, ch, !stereo);
            for p in 0..PAIRS {
                self.pushes.push(Push::Send(bus, ch & !1, p));
                self.pushes.push(Push::Send(bus, (ch & !1) + 1, p));
            }
            self.dirty(now);
        }
    }

    /// A strip's title: your label (double-click to rename) over the channel name.
    /// Returns true when it was single-clicked.
    fn name_box(&mut self, ui: &mut Ui, r: Rect, key: String, chan: &str, kind: &str, now: f64) -> bool {
        if let Some((k, text)) = &mut self.editing
            && *k == key
        {
            let resp = ui.put(r.shrink2(vec2(6.0, 8.0)), egui::TextEdit::singleline(text).font(mono(12.0)));
            resp.request_focus();
            let done = resp.lost_focus() || ui.input(|i| i.key_pressed(egui::Key::Enter));
            let cancel = ui.input(|i| i.key_pressed(egui::Key::Escape));
            if done || cancel {
                let t = text.trim().to_string();
                if !cancel {
                    if t.is_empty() {
                        self.st.labels.remove(&key);
                    } else {
                        self.st.labels.insert(key, t);
                    }
                    self.dirty(now);
                }
                self.editing = None;
            }
            return false;
        }
        let resp = ui
            .interact(r, Id::NULL.with(("name", key.as_str())), Sense::click())
            .on_hover_text("double-click to rename");
        let label = self.st.labels.get(&key);
        let (big, small) = match label {
            Some(l) => (l.as_str(), chan),
            None => (chan, kind),
        };
        let p = ui.painter();
        let big = elide(ui, big, mono(13.0), r.width() - 10.0);
        p.text(pos2(r.center().x, r.top() + 15.0), Align2::CENTER_CENTER, big, mono(13.0), FG);
        p.text(pos2(r.center().x, r.top() + 30.0), Align2::CENTER_CENTER, small, mono(10.0), MUTED);
        if resp.double_clicked() {
            self.editing = Some((key, label.cloned().unwrap_or_default()));
        }
        resp.clicked()
    }

    fn preamp(&mut self, ui: &mut Ui, r: Rect, i: usize, now: f64) {
        let id = Id::NULL.with(("pre", i));
        let (min, max) = self.dev.as_ref().map_or((0, DEMO_GAIN_MAX[i]), |d| d.gain_range(i));
        let available = self.dev.as_ref().is_none_or(|d| d.has_gain());
        let before = self.st.preamps[i];
        let pre = &mut self.st.preamps[i];

        let knob_c = pos2(r.center().x, r.top() + 24.0);
        let resp = knob_widget(ui, knob_c, 19.0, id.with("gain"), &mut pre.gain, min, max, available);
        let db = match &self.dev {
            Some(d) => d.gain_db(i, pre.gain),
            None => gain_db_fallback(i, pre.gain, max),
        };
        let hint = if available {
            "preamp gain: drag up/down, scroll, double-click for 0 dB"
        } else {
            "this kernel's driver doesn't expose preamp gain (needs Linux 6.4 or newer)"
        };
        resp.on_hover_text(hint);
        let p = ui.painter();
        let txt = if db.fract() == 0.0 { format!("{db:.0} dB") } else { format!("{db:.1} dB") };
        p.text(pos2(r.center().x, r.top() + 54.0), Align2::CENTER_CENTER, txt, mono(11.0), if available { FG } else { MUTED });

        let row = Rect::from_min_size(pos2(r.left() + 6.0, r.top() + 66.0), vec2(r.width() - 12.0, 22.0));
        if i < 2 {
            let half = (row.width() - 4.0) / 2.0;
            let a = Rect::from_min_size(row.min, vec2(half, row.height()));
            let b = Rect::from_min_size(pos2(row.left() + half + 4.0, row.top()), vec2(half, row.height()));
            if pill(ui, a, id.with("48v"), pre.phantom, "48V", RED)
                .on_hover_text("phantom power for condenser mics")
                .clicked()
            {
                pre.phantom = !pre.phantom;
            }
            if pill(ui, b, id.with("pad"), pre.pad, "PAD", ACCENT).on_hover_text("−20 dB pad for hot signals").clicked() {
                pre.pad = !pre.pad;
            }
        } else {
            let label = if pre.sens == 1 { "+4 dBu" } else { "-10 dBV" };
            if pill(ui, row, id.with("sens"), pre.sens == 1, label, ACCENT)
                .on_hover_text("input sensitivity: −10 dBV (consumer) or +4 dBu (pro line level)")
                .clicked()
            {
                pre.sens = 1 - pre.sens.clamp(0, 1);
            }
        }
        if *pre != before {
            self.pushes.push(Push::Preamp(i));
            self.dirty(now);
        }
    }

    fn take_screenshot_when_ready(&mut self, ctx: &egui::Context) {
        let Some(path) = self.opts.screenshot.clone() else { return };
        self.frames += 1;
        ctx.request_repaint();
        if self.frames != 10 {
            return;
        }
        let ctx2 = ctx.clone();
        ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::ScreenshotCallback::new(
            move |img| {
                match crate::png::write(&path, &img) {
                    Ok(()) => println!("saved {} ({}x{})", path.display(), img.size[0], img.size[1]),
                    Err(e) => eprintln!("couldn't save {}: {e}", path.display()),
                }
                ctx2.send_viewport_cmd(egui::ViewportCommand::Close);
            },
        )));
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let now = ui.input(|i| i.time);
        let ctx = ui.ctx().clone();
        self.poll_device(now);

        // 1–6 pick the output pair
        if self.editing.is_none() && !ctx.memory(|m| m.focused().is_some()) {
            let keys = [egui::Key::Num1, egui::Key::Num2, egui::Key::Num3, egui::Key::Num4, egui::Key::Num5, egui::Key::Num6];
            for (p, k) in keys.iter().enumerate() {
                if ui.input(|i| i.key_pressed(*k)) {
                    self.st.output = p;
                    self.dirty(now);
                }
            }
        }

        egui::Panel::top("top")
            .exact_size(BAR_H)
            .frame(egui::Frame::new().fill(CARD).stroke(Stroke::new(1.0, LINE)).inner_margin(egui::Margin::symmetric(14, 0)))
            .show(ui, |ui| self.top_bar(ui, now));

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(BG).inner_margin(12))
            .show(ui, |ui| {
                let avail = ui.available_height() - 10.0;
                // three rows with equal fader lengths; too short a window scrolls instead
                const ROW_GAP: f32 = 10.0;
                let chrome = 2.0 * CARD_PAD + 26.0 + 4.0; // padding, heading, strip margin
                let fixed = [NAME_H + PRE_H + PAN_H + FOOT_H, NAME_H + PAN_H + FOOT_H, NAME_H + FOOT_H];
                let total_fixed: f32 = fixed.iter().map(|f| f + chrome).sum::<f32>() + 2.0 * ROW_GAP;
                let fader = ((avail - total_fixed) / 3.0).max(MIN_FADER);
                let [in_h, pb_h, out_h] = fixed.map(|f| f + chrome + fader);
                let mut rows = |ui: &mut Ui| {
                    ui.spacing_mut().item_spacing.y = ROW_GAP;
                    self.card(ui, Bus::Input, in_h, now);
                    self.card(ui, Bus::Playback, pb_h, now);
                    self.outputs_card(ui, out_h, now);
                };
                if total_fixed + 3.0 * fader <= avail + 1.0 {
                    rows(ui);
                } else {
                    egui::ScrollArea::vertical().show(ui, |ui| rows(ui));
                }
            });

        self.flush_pushes();
        self.save_if_due(now, false);
        // keep listening for the card (and for changes made by other mixers)
        let wait = if self.dev.is_some() { 100 } else if self.dirty_at.is_some() { 300 } else { 500 };
        ctx.request_repaint_after(std::time::Duration::from_millis(wait));
        self.take_screenshot_when_ready(&ctx);
    }

    fn persist_egui_memory(&self) -> bool {
        false
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.save_if_due(f64::INFINITY, true);
    }
}

// --- widgets -----------------------------------------------------------------------

fn mono(size: f32) -> FontId {
    FontId::new(size, FontFamily::Monospace)
}

fn divider(p: &egui::Painter, strip: Rect, y: f32) {
    p.line_segment([pos2(strip.left() + 10.0, y), pos2(strip.right() - 10.0, y)], Stroke::new(1.0, LINE));
}

fn elide(ui: &Ui, s: &str, font: FontId, max_w: f32) -> String {
    let fits = |t: &str| ui.painter().layout_no_wrap(t.to_string(), font.clone(), FG).size().x <= max_w;
    if fits(s) {
        return s.to_string();
    }
    let mut chars: Vec<char> = s.chars().collect();
    while !chars.is_empty() {
        chars.pop();
        let t: String = chars.iter().collect::<String>() + "…";
        if fits(&t) {
            return t;
        }
    }
    String::new()
}

/// Mouse-wheel notches over a widget (+1 up, −1 down), consumed so nothing else scrolls.
fn wheel_steps(ui: &Ui, hovered: bool) -> f32 {
    if !hovered {
        return 0.0;
    }
    ui.input_mut(|i| {
        let mut n = 0.0;
        i.events.retain(|e| match e {
            egui::Event::MouseWheel { delta, .. } if delta.y != 0.0 => {
                n += delta.y.signum();
                false
            }
            _ => true,
        });
        n
    })
}

/// A small rounded toggle button.
fn pill(ui: &mut Ui, r: Rect, id: Id, on: bool, text: &str, on_col: Color32) -> egui::Response {
    let resp = ui.interact(r, id, Sense::click());
    let p = ui.painter();
    let (fill, stroke, tc) = if on {
        (on_col.gamma_multiply(0.28), on_col, on_col.lerp_to_gamma(FG, 0.35))
    } else if resp.hovered() {
        (LINE, MUTED, FG)
    } else {
        (CARD, LINE, MUTED)
    };
    p.rect_filled(r, 6, fill);
    p.rect_stroke(r, 6, Stroke::new(1.0, stroke), StrokeKind::Inside);
    p.text(r.center(), Align2::CENTER_CENTER, text, mono(11.0), tc);
    resp
}

/// Horizontal pan control: fills from the centre towards the side it's panned to.
fn pan_widget(ui: &mut Ui, r: Rect, id: Id, pan: &mut f32) {
    let resp = ui.interact(r, id, Sense::click_and_drag()).on_hover_text("pan: drag, scroll, double-click to centre");
    if resp.dragged() {
        let fine = if ui.input(|i| i.modifiers.shift) { 0.25 } else { 1.0 };
        *pan = (*pan + resp.drag_delta().x / (r.width() / 2.0) * fine).clamp(-1.0, 1.0);
    }
    let steps = wheel_steps(ui, resp.hovered());
    if steps != 0.0 {
        *pan = (*pan + steps * 0.05).clamp(-1.0, 1.0);
    }
    if resp.double_clicked() {
        *pan = 0.0;
    }
    if pan.abs() < 0.005 {
        *pan = 0.0;
    }
    let p = ui.painter();
    let track = Rect::from_center_size(r.center(), vec2(r.width(), 4.0));
    p.rect_filled(track, 2, LINE);
    let cx = track.center().x;
    let x = cx + *pan * track.width() / 2.0;
    let fill = Rect::from_min_max(pos2(cx.min(x), track.top()), pos2(cx.max(x), track.bottom()));
    p.rect_filled(fill, 2, ACCENT);
    p.line_segment([pos2(cx, r.top() + 1.0), pos2(cx, r.bottom() - 1.0)], Stroke::new(1.0, MUTED.gamma_multiply(0.5)));
    let active = resp.hovered() || resp.dragged();
    p.circle_filled(pos2(x, track.center().y), if active { 6.0 } else { 5.0 }, FG);
    if active {
        p.circle_stroke(pos2(x, track.center().y), 6.0, Stroke::new(1.5, ACCENT));
    }
}

/// Vertical fader with a dB scale; position ↔ level uses [`fader_to_vol`]'s taper.
fn fader_widget(ui: &mut Ui, r: Rect, id: Id, s: &mut Send) {
    let track = Rect::from_center_size(
        pos2(r.center().x + 6.0, r.center().y),
        vec2(6.0, (r.height() - 26.0).max(20.0)),
    );
    let len = track.height();
    let y_of = |pos: f32| track.bottom() - pos * len;

    let resp = ui.interact(r, id, Sense::click_and_drag()).on_hover_text(format!(
        "{} dB: drag, scroll (0.5 dB), double-click for 0 dB",
        fmt_db(vol_db(s.vol))
    ));
    // keep the exact fader position while dragging, so slow drags don't stall on rounding
    if resp.drag_started() {
        ui.data_mut(|d| d.insert_temp(id, vol_to_fader(s.vol)));
        // clicking the track (not the handle) jumps there
        if let Some(pt) = resp.interact_pointer_pos()
            && (pt.y - y_of(vol_to_fader(s.vol))).abs() > 10.0 {
                let pos = ((track.bottom() - pt.y) / len).clamp(0.0, 1.0);
                ui.data_mut(|d| d.insert_temp(id, pos));
            }
    }
    if resp.dragged() {
        let fine = if ui.input(|i| i.modifiers.shift) { 0.2 } else { 1.0 };
        let mut pos: f32 = ui.data(|d| d.get_temp(id)).unwrap_or(vol_to_fader(s.vol));
        pos = (pos - resp.drag_delta().y / len * fine).clamp(0.0, 1.0);
        ui.data_mut(|d| d.insert_temp(id, pos));
        s.vol = fader_to_vol(pos);
    }
    let steps = wheel_steps(ui, resp.hovered());
    if steps != 0.0 {
        let db = vol_db(s.vol);
        let db = if db.is_finite() { db + steps * 0.5 } else if steps > 0.0 { -60.0 } else { db };
        s.vol = if db < -60.0 { 0 } else { db_to_vol(db.min(6.02)) };
    }
    if resp.double_clicked() {
        s.vol = VOL_UNITY;
    }

    let p = ui.painter();
    // scale
    // labels top to bottom, skipping any that would crowd the one above (short faders);
    // 0 dB is always shown
    let mut last_label = f32::NEG_INFINITY;
    for db in [6.0, 0.0, -6.0, -12.0, -24.0, -48.0] {
        let y = y_of(vol_to_fader(db_to_vol(db)));
        let unity = db == 0.0;
        let c = if unity { MUTED } else { MUTED.gamma_multiply(0.45) };
        p.line_segment([pos2(track.left() - 9.0, y), pos2(track.left() - 4.0, y)], Stroke::new(1.0, c));
        let room = y - last_label >= 11.0 && (unity || y - y_of(vol_to_fader(VOL_UNITY)) >= 11.0 || db > 0.0)
            && (db <= 0.0 || y_of(vol_to_fader(VOL_UNITY)) - y >= 11.0);
        if r.width() >= 58.0 && room {
            last_label = y;
            let t = if db > 0.0 { format!("+{db:.0}") } else { format!("{}", (db as i32).abs()) };
            p.text(pos2(track.left() - 12.0, y), Align2::RIGHT_CENTER, t, mono(9.0), c);
        }
    }
    let ybot = y_of(0.0);
    if ybot - last_label >= 11.0 {
        p.text(pos2(track.left() - 12.0, ybot), Align2::RIGHT_CENTER, "∞", mono(9.0), MUTED.gamma_multiply(0.45));
    }

    // track and level fill
    p.rect_filled(track, 3, LINE);
    let pos = vol_to_fader(s.vol);
    let hy = y_of(pos);
    let fill_c = if s.mute { MUTED.gamma_multiply(0.5) } else { ACCENT };
    p.rect_filled(Rect::from_min_max(pos2(track.left(), hy), track.max), 3, fill_c);

    // handle
    let active = resp.hovered() || resp.dragged();
    let h = Rect::from_center_size(pos2(track.center().x, hy), vec2(28.0, 12.0));
    p.rect_filled(h.translate(vec2(0.0, 2.0)), 4, Color32::from_black_alpha(90));
    p.rect_filled(h, 4, if s.mute { MUTED } else { FG });
    p.line_segment([pos2(h.left() + 5.0, hy), pos2(h.right() - 5.0, hy)], Stroke::new(1.5, BG));
    if active {
        p.rect_stroke(h.expand(1.0), 5, Stroke::new(1.5, ACCENT), StrokeKind::Outside);
    }
}

/// A rotary knob drawn as a 270° arc. Drag up/down or scroll; double-click → `min`.
#[allow(clippy::too_many_arguments)]
fn knob_widget(ui: &mut Ui, c: Pos2, rad: f32, id: Id, val: &mut i64, min: i64, max: i64, enabled: bool) -> egui::Response {
    let r = Rect::from_center_size(c, vec2(rad * 2.0 + 8.0, rad * 2.0 + 8.0));
    let sense = if enabled { Sense::click_and_drag() } else { Sense::hover() };
    let resp = ui.interact(r, id, sense);
    let span = (max - min).max(1) as f32;
    if resp.drag_started() {
        ui.data_mut(|d| d.insert_temp(id, *val as f32));
    }
    if resp.dragged() {
        let fine = if ui.input(|i| i.modifiers.shift) { 0.25 } else { 1.0 };
        let mut v: f32 = ui.data(|d| d.get_temp(id)).unwrap_or(*val as f32);
        // 160 px of travel covers the whole range
        v = (v - resp.drag_delta().y * span / 160.0 * fine).clamp(min as f32, max as f32);
        ui.data_mut(|d| d.insert_temp(id, v));
        *val = v.round() as i64;
    }
    let steps = wheel_steps(ui, enabled && resp.hovered());
    if steps != 0.0 {
        *val = (*val + steps as i64).clamp(min, max);
    }
    if enabled && resp.double_clicked() {
        *val = min;
    }

    let p = ui.painter();
    let frac = ((*val - min) as f32 / span).clamp(0.0, 1.0);
    // angles in screen space (y down): start bottom-left, sweep clockwise to bottom-right
    let a0 = std::f32::consts::PI * 0.75;
    let sweep = std::f32::consts::PI * 1.5;
    let arc = |from: f32, to: f32| -> Vec<Pos2> {
        let n = ((to - from).abs() * 24.0).ceil().max(2.0) as usize;
        (0..=n)
            .map(|k| {
                let a = from + (to - from) * k as f32 / n as f32;
                c + vec2(a.cos(), a.sin()) * rad
            })
            .collect()
    };
    p.line(arc(a0, a0 + sweep), Stroke::new(4.0, LINE));
    let active = enabled && (resp.hovered() || resp.dragged());
    if frac > 0.0 {
        let col = if enabled { ACCENT } else { MUTED };
        p.line(arc(a0, a0 + sweep * frac), Stroke::new(4.0, col));
    }
    p.circle_filled(c, rad - 6.0, if active { Color32::from_rgb(0x2a, 0x2a, 0x30) } else { CARD });
    p.circle_stroke(c, rad - 6.0, Stroke::new(1.0, LINE));
    let a = a0 + sweep * frac;
    let dir = vec2(a.cos(), a.sin());
    p.line_segment([c + dir * (rad - 13.0), c + dir * (rad - 7.0)], Stroke::new(2.0, if enabled { FG } else { MUTED }));
    let _ = CornerRadius::ZERO;
    resp
}

fn apply_style(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();
    v.panel_fill = BG;
    v.window_fill = CARD;
    v.window_stroke = Stroke::new(1.0, LINE);
    v.extreme_bg_color = BG;
    v.faint_bg_color = CARD;
    v.override_text_color = Some(FG);
    v.selection.bg_fill = ACCENT.gamma_multiply(0.35);
    v.selection.stroke = Stroke::new(1.0, ACCENT);
    v.hyperlink_color = ACCENT;
    let r = CornerRadius::same(6);
    for (w, fill, stroke) in [
        (&mut v.widgets.noninteractive, BG, LINE),
        (&mut v.widgets.inactive, CARD, LINE),
        (&mut v.widgets.hovered, CARD, ACCENT),
        (&mut v.widgets.active, LINE, ACCENT),
        (&mut v.widgets.open, CARD, ACCENT),
    ] {
        w.bg_fill = fill;
        w.weak_bg_fill = fill;
        w.bg_stroke = Stroke::new(1.0, stroke);
        w.corner_radius = r;
        w.fg_stroke.color = FG;
    }
    v.widgets.noninteractive.fg_stroke.color = MUTED;
    v.widgets.hovered.fg_stroke.color = ACCENT;
    ctx.all_styles_mut(|s| {
        s.visuals = v.clone();
        for (style, font) in s.text_styles.iter_mut() {
            let k = match style {
                egui::TextStyle::Heading => 1.25,
                egui::TextStyle::Small => 0.8,
                _ => 1.0,
            };
            *font = FontId::new(13.0 * k, FontFamily::Monospace);
        }
        s.spacing.item_spacing = vec2(8.0, 6.0);
        s.spacing.button_padding = vec2(8.0, 3.0);
    });
}
