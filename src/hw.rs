//! A thin, safe-ish wrapper around the ALSA control interface (`snd_ctl_*`).
//!
//! We talk to the card's controls by name ("Mic-AN1-AN1 Playback Volume", "Mic-AN1 Gain", …)
//! the same way `amixer cset` does, which gives us the real ranges and enum labels,
//! and lets us subscribe to change events so other tools (alsamixer, alsactl) stay in sync.

use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::ptr;

use alsa_sys::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Bool,
    Int,
    Enum,
    Other,
}

#[derive(Debug, Clone)]
pub struct Control {
    pub numid: u32,
    pub kind: Kind,
    pub count: u32,
    pub min: i64,
    pub max: i64,
    pub items: Vec<String>,
}

pub struct Card {
    ctl: *mut snd_ctl_t,
    pub name: String,
    pub index: i32,
    by_name: HashMap<String, Control>,
}

/// Something went wrong talking to the card: most likely it was unplugged.
#[derive(Debug)]
pub struct Gone;

impl Card {
    /// Finds the first card whose name contains "Babyface Pro" and opens its control device.
    pub fn find_babyface() -> Option<Card> {
        let mut n: i32 = -1;
        loop {
            if unsafe { snd_card_next(&mut n) } < 0 || n < 0 {
                return None;
            }
            if let Some(c) = Self::open(n)
                && c.name.contains("Babyface Pro") {
                    return Some(c);
                }
        }
    }

    fn open(index: i32) -> Option<Card> {
        let dev = CString::new(format!("hw:{index}")).ok()?;
        let mut ctl = ptr::null_mut();
        if unsafe { snd_ctl_open(&mut ctl, dev.as_ptr(), SND_CTL_NONBLOCK) } < 0 {
            return None;
        }
        let name = unsafe {
            let mut info = ptr::null_mut();
            snd_ctl_card_info_malloc(&mut info);
            let name = if snd_ctl_card_info(ctl, info) >= 0 {
                cstr(snd_ctl_card_info_get_name(info))
            } else {
                String::new()
            };
            snd_ctl_card_info_free(info);
            name
        };
        let mut card = Card { ctl, name, index, by_name: HashMap::new() };
        if card.name.contains("Babyface Pro") {
            card.load_controls();
            unsafe { snd_ctl_subscribe_events(ctl, 1) };
        }
        Some(card)
    }

    fn load_controls(&mut self) {
        unsafe {
            let mut list = ptr::null_mut();
            snd_ctl_elem_list_malloc(&mut list);
            if snd_ctl_elem_list(self.ctl, list) < 0 {
                snd_ctl_elem_list_free(list);
                return;
            }
            let count = snd_ctl_elem_list_get_count(list);
            snd_ctl_elem_list_alloc_space(list, count);
            snd_ctl_elem_list(self.ctl, list);
            let mut info = ptr::null_mut();
            snd_ctl_elem_info_malloc(&mut info);
            for i in 0..snd_ctl_elem_list_get_used(list) {
                let numid = snd_ctl_elem_list_get_numid(list, i);
                let name = cstr(snd_ctl_elem_list_get_name(list, i));
                snd_ctl_elem_info_set_numid(info, numid);
                if snd_ctl_elem_info(self.ctl, info) < 0 {
                    continue;
                }
                let kind = match snd_ctl_elem_info_get_type(info) {
                    SND_CTL_ELEM_TYPE_BOOLEAN => Kind::Bool,
                    SND_CTL_ELEM_TYPE_INTEGER => Kind::Int,
                    SND_CTL_ELEM_TYPE_ENUMERATED => Kind::Enum,
                    _ => Kind::Other,
                };
                let (mut min, mut max, mut items) = (0, 1, Vec::new());
                match kind {
                    Kind::Int => {
                        min = snd_ctl_elem_info_get_min(info) as i64;
                        max = snd_ctl_elem_info_get_max(info) as i64;
                    }
                    Kind::Enum => {
                        let n = snd_ctl_elem_info_get_items(info);
                        for j in 0..n {
                            snd_ctl_elem_info_set_item(info, j);
                            snd_ctl_elem_info(self.ctl, info);
                            items.push(cstr(snd_ctl_elem_info_get_item_name(info)));
                        }
                        max = n as i64 - 1;
                    }
                    _ => {}
                }
                let count = snd_ctl_elem_info_get_count(info);
                let c = Control { numid, kind, count, min, max, items };
                self.by_name.insert(name, c);
            }
            snd_ctl_elem_info_free(info);
            snd_ctl_elem_list_free_space(list);
            snd_ctl_elem_list_free(list);
        }
    }

    /// Looks a control up by its base name, with or without the usual ALSA suffixes.
    pub fn control(&self, base: &str) -> Option<&Control> {
        ["", " Switch", " Playback Volume", " Playback Switch", " Volume"]
            .iter()
            .find_map(|s| self.by_name.get(&format!("{base}{s}")))
    }

    pub fn read(&self, c: &Control) -> Result<i64, Gone> {
        unsafe {
            let mut v = ptr::null_mut();
            snd_ctl_elem_value_malloc(&mut v);
            snd_ctl_elem_value_set_numid(v, c.numid);
            let err = snd_ctl_elem_read(self.ctl, v);
            let out = match c.kind {
                Kind::Bool => snd_ctl_elem_value_get_boolean(v, 0) as i64,
                Kind::Enum => snd_ctl_elem_value_get_enumerated(v, 0) as i64,
                _ => snd_ctl_elem_value_get_integer(v, 0) as i64,
            };
            snd_ctl_elem_value_free(v);
            if err < 0 { Err(Gone) } else { Ok(out) }
        }
    }

    /// Writes `val` (clamped to the control's range) to every channel of the control.
    pub fn write(&self, c: &Control, val: i64) -> Result<(), Gone> {
        let val = val.clamp(c.min, c.max);
        unsafe {
            let mut v = ptr::null_mut();
            snd_ctl_elem_value_malloc(&mut v);
            snd_ctl_elem_value_set_numid(v, c.numid);
            for i in 0..c.count.max(1) {
                match c.kind {
                    Kind::Bool => snd_ctl_elem_value_set_boolean(v, i, (val != 0) as _),
                    Kind::Enum => snd_ctl_elem_value_set_enumerated(v, i, val as _),
                    _ => snd_ctl_elem_value_set_integer(v, i, val as _),
                }
            }
            let err = snd_ctl_elem_write(self.ctl, v);
            snd_ctl_elem_value_free(v);
            // -EBUSY/-EPERM etc. are not fatal; a vanished device is
            if err == -libc_enodev() { Err(Gone) } else { Ok(()) }
        }
    }

    /// The control's own dB value for `val`, if the driver publishes a dB scale (TLV).
    pub fn db(&self, c: &Control, val: i64) -> Option<f32> {
        unsafe {
            let mut id = ptr::null_mut();
            snd_ctl_elem_id_malloc(&mut id);
            snd_ctl_elem_id_set_numid(id, c.numid);
            let mut out: std::ffi::c_long = 0;
            let r = snd_ctl_convert_to_dB(self.ctl, id, val as _, &mut out);
            snd_ctl_elem_id_free(id);
            (r >= 0).then_some(out as f32 / 100.0)
        }
    }

    /// Numids of controls whose value changed since the last call. `Err` means the card is gone.
    pub fn changes(&self) -> Result<Vec<u32>, Gone> {
        let mut out = Vec::new();
        unsafe {
            let mut ev = ptr::null_mut();
            snd_ctl_event_malloc(&mut ev);
            let res = loop {
                let r = snd_ctl_read(self.ctl, ev);
                if r == 0 || r == -libc_eagain() {
                    break Ok(());
                }
                if r < 0 {
                    break Err(Gone);
                }
                if snd_ctl_event_get_type(ev) == SND_CTL_EVENT_ELEM {
                    let mask = snd_ctl_event_elem_get_mask(ev);
                    if mask == !0u32 {
                        break Err(Gone); // element removed: device unplugged
                    }
                    if mask & SND_CTL_EVENT_MASK_VALUE != 0 {
                        out.push(snd_ctl_event_elem_get_numid(ev));
                    }
                }
            };
            snd_ctl_event_free(ev);
            res?;
        }
        out.sort_unstable();
        out.dedup();
        Ok(out)
    }
}

impl Drop for Card {
    fn drop(&mut self) {
        unsafe { snd_ctl_close(self.ctl) };
    }
}

fn cstr(p: *const std::ffi::c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

const SND_CTL_NONBLOCK: i32 = 1;
const SND_CTL_EVENT_MASK_VALUE: u32 = 1;

// errno values on Linux (avoids pulling in the libc crate for two numbers)
const fn libc_eagain() -> i32 {
    11
}
const fn libc_enodev() -> i32 {
    19
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Read-only smoke test of the FFI against whatever cards this machine has:
    /// `cargo test -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn reads_controls_of_every_card() {
        let mut n: i32 = -1;
        while unsafe { snd_card_next(&mut n) } >= 0 && n >= 0 {
            let Some(mut c) = Card::open(n) else { continue };
            c.load_controls();
            unsafe { snd_ctl_subscribe_events(c.ctl, 1) };
            println!("hw:{n} {:?}: {} controls", c.name, c.by_name.len());
            let mut names: Vec<_> = c.by_name.keys().cloned().collect();
            names.sort();
            for name in names {
                let ctl = &c.by_name[&name];
                let v = c.read(ctl).expect("read");
                let db = (ctl.kind == Kind::Int).then(|| c.db(ctl, v)).flatten();
                println!("  {name:40} {:?} {}..{} = {v} {db:?} {:?}", ctl.kind, ctl.min, ctl.max, ctl.items);
            }
            assert!(c.changes().is_ok());
        }
    }
}
