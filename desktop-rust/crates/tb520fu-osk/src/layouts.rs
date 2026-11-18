// SPDX-License-Identifier: MIT
use serde::{Deserialize, Serialize};
use tb520fu_ui::canvas::RectF;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum KbMode {
    #[default]
    Dock,
    Split,
    Float,
}

impl KbMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dock => "dock",
            Self::Split => "split",
            Self::Float => "float",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum KbLayout {
    #[default]
    Main,
    Num,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Lang {
    #[default]
    Zh,
    En,
    Ja,
}

impl Lang {
    pub fn is_cjk(self) -> bool {
        matches!(self, Self::Zh | Self::Ja)
    }

    pub fn next(self) -> Self {
        match self {
            Self::Zh => Self::En,
            Self::En => Self::Zh, // Toggle Zh <-> En directly (most ergonomic on Chinese tablet; Ja reachable in LayoutPicker)
            Self::Ja => Self::Zh,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyKind {
    Char,
    Punct,
    Digit,
    Del,
    Space,
    Enter,
    Shift,
    Sep,
    Sym,
    Num,
    Back,
    Lang,
    List,
}

#[derive(Debug, Clone)]
pub struct KeyDef {
    pub id: &'static str,
    pub k: &'static str,
    pub kind: KeyKind,
    pub x: f32,
    pub w: f32,
    pub row: usize,
    pub row_span: usize,
    pub fn_tint: bool,
    pub send: bool,
    pub ch: &'static str,
    pub rect: RectF,
}

#[derive(Debug, Clone, Copy)]
pub struct KbMetrics {
    pub u: f32,
    pub fs: f32,
    pub band_h: f32,
    pub key_h: f32,
    pub bot_h: f32,
    pub kb_w: f32,
    pub kb_h: f32,
    pub fpad: f32,
    pub surf_w: u32,
    pub surf_h: u32,
    pub portrait: bool,
    pub split_half_frac: f32,
}

impl KbMetrics {
    pub fn compute(mode: KbMode, hadj: f32, screen_w: u32, screen_h: u32) -> Self {
        let sw = (screen_w.max(480)) as f32;
        let sh = (screen_h.max(360)) as f32;
        let portrait = sw < sh;
        let u = (sw.min(sh) / 1003.0).clamp(0.80, 1.25);
        let hadj = hadj.clamp(0.80, 1.25);

        let (design_w, band, key_h_base, bot, fs) = match mode {
            KbMode::Float => (485.0, 61.0, 279.0 * hadj, 30.0, 0.83),
            KbMode::Split => (
                sw / u,
                76.0,
                (if portrait { 301.0 } else { 296.0 }) * hadj,
                16.0,
                0.94,
            ),
            KbMode::Dock => (
                sw / u,
                76.0,
                (if portrait { 301.0 } else { 296.0 }) * hadj,
                16.0,
                1.0,
            ),
        };

        let band_h = (band * u).round();
        let key_h = (key_h_base * u).round();
        let bot_h = (bot * u).round();
        let kb_h = band_h + key_h + bot_h;
        let kb_w = if mode == KbMode::Float {
            (design_w * u).round()
        } else {
            sw
        };
        let fpad = if mode == KbMode::Float { (20.0 * u).round() } else { 0.0 };
        let surf_w = (kb_w + 2.0 * fpad).round() as u32;
        let surf_h = (kb_h + 2.0 * fpad).round() as u32;
        let split_half_frac = if portrait { 258.0 / 640.0 } else { 265.0 / 1024.0 };

        Self {
            u,
            fs,
            band_h,
            key_h,
            bot_h,
            kb_w,
            kb_h,
            fpad,
            surf_w,
            surf_h,
            portrait,
            split_half_frac,
        }
    }

    /// Compute click-through `wl_region` rectangles in surface-local logical coordinates.
    pub fn input_regions(&self, mode: KbMode, split_glass_active: bool, slide_y: f32) -> Vec<RectF> {
        let bx = self.fpad;
        let by = self.fpad + slide_y.max(0.0);
        let bh = (self.kb_h - slide_y.max(0.0)).max(0.0);
        if bh <= 1.0 {
            return Vec::new();
        }
        if mode == KbMode::Float {
            vec![RectF::new(bx, by, self.kb_w, bh)]
        } else if split_glass_active {
            let half_w = (self.kb_w * self.split_half_frac).ceil();
            let top_h = self.band_h.min(bh);
            let rem_h = (bh - top_h).max(0.0);
            let mut out = vec![RectF::new(bx, by, self.kb_w, top_h)];
            if rem_h > 0.0 {
                out.push(RectF::new(bx, by + top_h, half_w, rem_h));
                out.push(RectF::new(bx + self.kb_w - half_w, by + top_h, half_w, rem_h));
            }
            out
        } else {
            vec![RectF::new(bx, by, self.kb_w, bh)]
        }
    }
}

pub fn hint_for(k: &str, zh: bool) -> Option<&'static str> {
    if zh {
        match k {
            "q" => Some("1"), "w" => Some("2"), "e" => Some("3"), "r" => Some("4"), "t" => Some("5"),
            "y" => Some("6"), "u" => Some("7"), "i" => Some("8"), "o" => Some("9"), "p" => Some("0"),
            "a" => Some("~"), "s" => Some("!"), "d" => Some("@"), "f" => Some("#"), "g" => Some("%"),
            "h" => Some("“"), "j" => Some("”"), "k" => Some("*"), "l" => Some("?"),
            "z" => Some("("), "x" => Some(")"), "c" => Some("-"), "v" => Some("_"), "b" => Some("："),
            "n" => Some("；"), "m" => Some("/"),
            _ => None,
        }
    } else {
        match k {
            "q" => Some("1"), "w" => Some("2"), "e" => Some("3"), "r" => Some("4"), "t" => Some("5"),
            "y" => Some("6"), "u" => Some("7"), "i" => Some("8"), "o" => Some("9"), "p" => Some("0"),
            "a" => Some("~"), "s" => Some("!"), "d" => Some("@"), "f" => Some("#"), "g" => Some("%"),
            "h" => Some("'"), "j" => Some("&"), "k" => Some("*"), "l" => Some("?"),
            "z" => Some("("), "x" => Some(")"), "c" => Some("-"), "v" => Some("_"), "b" => Some(":"),
            "n" => Some(";"), "m" => Some("\\"),
            _ => None,
        }
    }
}

pub fn alternates_for(k: &str, zh: bool) -> Vec<String> {
    let upper = k.to_ascii_uppercase();
    let lower = k.to_string();
    let hint = hint_for(k, zh).unwrap_or("").to_string();
    let mut items = if k == "a" || k == "q" {
        vec![hint, upper, lower]
    } else if k == "l" || k == "p" {
        vec![upper, lower, hint]
    } else {
        vec![upper, hint, lower]
    };
    items.retain(|s| !s.is_empty());
    if !zh {
        let accents: &[&str] = match k {
            "a" => &["à", "á", "â", "ä"],
            "e" => &["è", "é", "ê", "ë"],
            "i" => &["ì", "í", "î", "ï"],
            "o" => &["ò", "ó", "ô", "ö"],
            "u" => &["ù", "ú", "û", "ü"],
            "c" => &["ç"],
            "n" => &["ñ"],
            "y" => &["ÿ"],
            _ => &[],
        };
        for &a in accents {
            items.push(a.to_string());
        }
    }
    items
}

pub const NUM_LIST: &[&str] = &[
    "@", "%", "/", "-", "+", "*", ":", "#", "_", "(", ")", "=", "¥", "$",
];

pub const SYM_CATS: &[(&str, &[&str])] = &[
    (
        "中文",
        &[
            "，", "。", "？", "！", "～", "、", "：", "#", "；", "％", "*", "——", "……", "&", "·",
            "$", "（", "）", "‘", "’", "“", "”", "［", "］", "『", "』", "〔", "〕", "{", "}",
            "【", "】", "¥", "￡", "‖", "〖", "〗", "《", "》", "「", "」", "｜", "〈", "〉",
        ],
    ),
    (
        "英文",
        &[
            "!", "@", "#", "$", "%", "^", "&", "*", "(", ")", "-", "_", "=", "+", "[", "]", "{",
            "}", "\\", "|", ";", ":", "'", "\"", ",", ".", "<", ">", "/", "?", "~", "`",
        ],
    ),
    (
        "网络",
        &[
            ".", ":", "/", "@", "-", "_", "http://", "https://", "www.", ".com", ".cn", ".net",
            ".org", ".io", "ssh://",
        ],
    ),
    (
        "数学",
        &[
            "+", "−", "×", "÷", "=", "≠", "≈", "±", "≤", "≥", "<", ">", "%", "‰", "√", "∞", "∑",
            "∏", "∫", "∠", "⊥", "∥", "∵", "∴", "°", "′", "″", "π", "∈", "∉", "⊆", "∪", "∩",
        ],
    ),
    (
        "序号",
        &[
            "①", "②", "③", "④", "⑤", "⑥", "⑦", "⑧", "⑨", "⑩", "⑴", "⑵", "⑶", "⑷", "⑸", "⑹", "⑺",
            "⑻", "⑼", "⑽", "Ⅰ", "Ⅱ", "Ⅲ", "Ⅳ", "Ⅴ", "Ⅵ", "Ⅶ", "Ⅷ", "Ⅸ", "Ⅹ",
        ],
    ),
    (
        "箭头",
        &[
            "←", "↑", "→", "↓", "↖", "↗", "↘", "↙", "↔", "↕", "⇐", "⇒", "⇑", "⇓", "⇔", "⟵", "⟶",
            "➜", "➔", "↩", "↪", "⤴", "⤵",
        ],
    ),
    (
        "特殊",
        &[
            "★", "☆", "○", "●", "◎", "◇", "◆", "□", "■", "△", "▲", "※", "§", "№", "♪", "♫", "✓",
            "✗", "℃", "℉", "©", "®", "™", "€", "…", "·", "–",
        ],
    ),
];

pub const EMOJI_CATS: &[(&str, &str)] = &[
    ("最近", "😂🙏👍😊❤️🤣😭😘🥰👌🎉🔥😅🤔😁👏💪🙌😎🥳😴🤗😋🌹☕🍜🚗✅"),
    ("表情", "😀😃😄😁😆😅🤣😂🙂🙃😉😊😇🥰😍🤩😘😗😚😋😛😜🤪😝🤑🤗🤭🤫🤔🤐🤨😐😑😶😏😒🙄😬😌😔😪🤤😴😷🤒🤕🤢🤮🥵🥶🥴😵🤯🤠🥳😎🤓🧐😕😟🙁😮😯😲😳🥺😢😭😱😤😡😠👋🤚✋👌✌️🤞👍👎✊👊👏🙌🤝🙏💪"),
    ("自然", "🐶🐱🐭🐹🐰🦊🐻🐼🐨🐯🦁🐮🐷🐸🐵🐔🐧🐦🐤🦆🦅🦉🐺🐴🦄🐝🦋🐢🐍🐙🦀🐬🐳🦈🌸🌹🌻🌲🍀🍁🌙⭐☀️🌈❄️"),
    ("美食", "🍏🍎🍐🍊🍋🍌🍉🍇🍓🍒🍑🥭🍍🥝🍅🥑🥦🍄🍞🥐🧀🥚🍳🥓🥩🍗🍔🍟🍕🌭🥪🌮🍜🍲🍛🍣🍱🥟🍤🍙🍚🍧🍨🍦🍰🎂🍮🍭🍬🍫🍿🍩🍪☕🍵🧋🥤🍺🍻🥂🍷"),
    ("活动", "⚽🏀🏈⚾🎾🏐🏉🎱🏓🏸🥅⛳🏹🎣🥊🎿🏂🏋️🏊🚴🎮🕹️🎲🧩🎯🎳🎸🎹🎺🎻🥁🎤🎧🎬🎨"),
    ("物品", "⌚📱💻⌨️🖥️🖨️🖱️💾📷📸📹🎥📞📺⏰⏳🔋🔌💡🔦💰💳💎🔧🔨⚙️🧰🔑🚪🎁🎈📦📝📚📖🔖🔗📎✂️📌📍"),
];

const QWERTY: [&str; 26] = [
    "q", "w", "e", "r", "t", "y", "u", "i", "o", "p", "a", "s", "d", "f", "g", "h", "j", "k", "l",
    "z", "x", "c", "v", "b", "n", "m",
];

#[allow(clippy::too_many_arguments)]
fn mk(
    id: &'static str,
    k: &'static str,
    kind: KeyKind,
    x: f32,
    w: f32,
    row: usize,
    row_span: usize,
    fn_tint: bool,
    send: bool,
    ch: &'static str,
) -> KeyDef {
    KeyDef {
        id,
        k,
        kind,
        x,
        w,
        row,
        row_span,
        fn_tint,
        send,
        ch,
        rect: RectF::default(),
    }
}

fn sep_key(zh: bool, ja: bool, x: f32, w: f32) -> KeyDef {
    if ja {
        mk("chouon", "chouon", KeyKind::Punct, x, w, 2, 1, true, false, "ー")
    } else if zh {
        mk("sep", "sep", KeyKind::Sep, x, w, 2, 1, true, false, "")
    } else {
        mk("shift", "shift", KeyKind::Shift, x, w, 2, 1, true, false, "")
    }
}

fn comma_ch(zh: bool, ja: bool) -> &'static str {
    if ja { "、" } else if zh { "，" } else { "," }
}

fn period_ch(zh: bool) -> &'static str {
    if zh { "。" } else { "." }
}

/// Build positioned `KeyDef` list in keyboard-body-local coordinates (`x` in `0..kb_w`, `y` in `band_h..band_h+key_h`).
pub fn build_keys(mode: KbMode, layout: KbLayout, lang: Lang, m: &KbMetrics) -> Vec<KeyDef> {
    let zh = lang.is_cjk();
    let ja = lang == Lang::Ja;
    let (mut defs, cw, ch, ry, kh): (Vec<KeyDef>, f32, f32, [f32; 4], f32) = if layout == KbLayout::Num {
        let mut l = Vec::new();
        let xs = [195.0, 409.0, 623.0];
        const DIGS: [(&str, &str); 9] = [
            ("d1", "1"), ("d2", "2"), ("d3", "3"),
            ("d4", "4"), ("d5", "5"), ("d6", "6"),
            ("d7", "7"), ("d8", "8"), ("d9", "9"),
        ];
        l.push(mk("list", "list", KeyKind::List, 8.0, 179.0, 0, 3, true, false, ""));
        for (idx, &(id, dc)) in DIGS.iter().enumerate() {
            l.push(mk(id, id, KeyKind::Digit, xs[idx % 3], 206.0, idx / 3, 1, false, false, dc));
        }
        l.push(mk("del", "del", KeyKind::Del, 837.0, 179.0, 0, 1, true, false, ""));
        l.push(mk("dot", "dot", KeyKind::Digit, 837.0, 179.0, 1, 1, true, false, "."));
        l.push(mk("at", "at", KeyKind::Digit, 837.0, 179.0, 2, 1, true, false, "@"));
        l.push(mk("sym", "sym", KeyKind::Sym, 8.0, 179.0, 3, 1, true, false, ""));
        l.push(mk("back", "back", KeyKind::Back, 195.0, 129.0, 3, 1, true, false, ""));
        l.push(mk("d0", "d0", KeyKind::Digit, 332.0, 360.0, 3, 1, false, false, "0"));
        l.push(mk("space", "space", KeyKind::Space, 700.0, 129.0, 3, 1, true, false, " "));
        l.push(mk("enter", "enter", KeyKind::Enter, 837.0, 179.0, 3, 1, true, false, ""));
        (l, 1024.0, 222.0, [4.0, 60.0, 116.0, 172.0], 50.0)
    } else if mode == KbMode::Float {
        let mut l = Vec::new();
        for (i, &k) in QWERTY[0..10].iter().enumerate() {
            l.push(mk(k, k, KeyKind::Char, 5.0 + 36.0 * (i as f32), 30.0, 0, 1, false, false, ""));
        }
        for (i, &k) in QWERTY[10..19].iter().enumerate() {
            l.push(mk(k, k, KeyKind::Char, 23.0 + 36.0 * (i as f32), 30.0, 1, 1, false, false, ""));
        }
        l.push(sep_key(zh, ja, 5.0, 48.0));
        for (i, &k) in QWERTY[19..26].iter().enumerate() {
            l.push(mk(k, k, KeyKind::Char, 59.0 + 36.0 * (i as f32), 30.0, 2, 1, false, false, ""));
        }
        l.push(mk("del", "del", KeyKind::Del, 311.0, 48.0, 2, 1, true, false, ""));
        l.push(mk("sym", "sym", KeyKind::Sym, 5.0, 48.0, 3, 1, true, false, ""));
        l.push(mk("123", "123", KeyKind::Num, 59.0, 39.0, 3, 1, true, false, ""));
        l.push(mk("comma", "comma", KeyKind::Punct, 104.0, 30.0, 3, 1, false, false, comma_ch(zh, ja)));
        l.push(mk("space", "space", KeyKind::Space, 140.0, 86.0, 3, 1, false, false, " "));
        l.push(mk("period", "period", KeyKind::Punct, 232.0, 30.0, 3, 1, false, false, period_ch(zh)));
        l.push(mk("lang", "lang", KeyKind::Lang, 268.0, 37.0, 3, 1, true, false, ""));
        l.push(mk("enter", "enter", KeyKind::Enter, 311.0, 48.0, 3, 1, true, true, ""));
        (l, 364.0, 210.0, [3.0, 57.0, 111.0, 165.0], 44.0)
    } else if mode == KbMode::Split {
        let mut l = Vec::new();
        let p = 46.0;
        let w = 42.0;
        for (i, &k) in QWERTY[0..5].iter().enumerate() {
            l.push(mk(k, k, KeyKind::Char, 8.0 + p * (i as f32), w, 0, 1, false, false, ""));
        }
        for (i, &k) in QWERTY[5..10].iter().enumerate() {
            l.push(mk(k, k, KeyKind::Char, 790.0 + p * (i as f32), w, 0, 1, false, false, ""));
        }
        for (i, &k) in QWERTY[10..15].iter().enumerate() {
            l.push(mk(k, k, KeyKind::Char, 29.0 + p * (i as f32), w, 1, 1, false, false, ""));
        }
        // Right half includes duplicate ergonomic 'g' (`g2`), matching OriginOS split pad!
        let right_r1 = [("g2", "g"), ("h", "h"), ("j", "j"), ("k", "k"), ("l", "l")];
        for (i, &(id, k)) in right_r1.iter().enumerate() {
            l.push(mk(id, k, KeyKind::Char, 767.0 + p * (i as f32), w, 1, 1, false, false, ""));
        }
        l.push(sep_key(zh, ja, 8.0, 63.0));
        for (i, &k) in QWERTY[19..23].iter().enumerate() {
            l.push(mk(k, k, KeyKind::Char, 75.0 + p * (i as f32), w, 2, 1, false, false, ""));
        }
        // Right half includes duplicate ergonomic 'v' (`v2`)
        let right_r2 = [("v2", "v"), ("b", "b"), ("n", "n"), ("m", "m")];
        for (i, &(id, k)) in right_r2.iter().enumerate() {
            l.push(mk(id, k, KeyKind::Char, 769.0 + p * (i as f32), w, 2, 1, false, false, ""));
        }
        l.push(mk("del", "del", KeyKind::Del, 953.0, 63.0, 2, 1, true, false, ""));
        l.push(mk("sym", "sym", KeyKind::Sym, 8.0, 64.0, 3, 1, true, false, ""));
        l.push(mk("123", "123", KeyKind::Num, 76.0, 42.0, 3, 1, true, false, ""));
        l.push(mk("comma", "comma", KeyKind::Punct, 122.0, 44.0, 3, 1, false, false, comma_ch(zh, ja)));
        l.push(mk("space", "space", KeyKind::Space, 170.0, 86.0, 3, 1, false, false, " "));
        l.push(mk("space2", "space", KeyKind::Space, 768.0, 88.0, 3, 1, false, false, " "));
        l.push(mk("period", "period", KeyKind::Punct, 860.0, 42.0, 3, 1, false, false, period_ch(zh)));
        l.push(mk("lang", "lang", KeyKind::Lang, 906.0, 42.0, 3, 1, true, false, ""));
        l.push(mk("enter", "enter", KeyKind::Enter, 952.0, 64.0, 3, 1, true, true, ""));

        if m.portrait {
            for d in &mut l {
                if d.x >= 512.0 {
                    d.x -= 384.0;
                }
            }
            (l, 640.0, 222.0, [4.0, 60.0, 116.0, 172.0], 50.0)
        } else {
            (l, 1024.0, 222.0, [4.0, 60.0, 116.0, 172.0], 50.0)
        }
    } else if m.portrait {
        let mut l = Vec::new();
        for (i, &k) in QWERTY[0..10].iter().enumerate() {
            l.push(mk(k, k, KeyKind::Char, 8.0 + 64.0 * (i as f32), 56.0, 0, 1, false, false, ""));
        }
        for (i, &k) in QWERTY[10..19].iter().enumerate() {
            l.push(mk(k, k, KeyKind::Char, 36.0 + 64.0 * (i as f32), 56.0, 1, 1, false, false, ""));
        }
        l.push(sep_key(zh, ja, 8.0, 84.0));
        for (i, &k) in QWERTY[19..26].iter().enumerate() {
            l.push(mk(k, k, KeyKind::Char, 100.0 + 64.0 * (i as f32), 56.0, 2, 1, false, false, ""));
        }
        l.push(mk("del", "del", KeyKind::Del, 548.0, 84.0, 2, 1, true, false, ""));
        if zh {
            l.push(mk("sym", "sym", KeyKind::Sym, 8.0, 84.0, 3, 1, true, false, ""));
            l.push(mk("123", "123", KeyKind::Num, 100.0, 59.0, 3, 1, true, false, ""));
        } else {
            l.push(mk("sym", "sym", KeyKind::Sym, 8.0, 38.0, 3, 1, true, false, ""));
            l.push(mk("123", "123", KeyKind::Num, 54.0, 38.0, 3, 1, true, false, ""));
            l.push(mk("at", "at", KeyKind::Punct, 100.0, 59.0, 3, 1, false, false, "@"));
        }
        l.push(mk("comma", "comma", KeyKind::Punct, 167.0, 59.0, 3, 1, false, false, comma_ch(zh, ja)));
        l.push(mk("space", "space", KeyKind::Space, 234.0, 172.0, 3, 1, false, false, " "));
        l.push(mk("period", "period", KeyKind::Punct, 414.0, 59.0, 3, 1, false, false, period_ch(zh)));
        l.push(mk("lang", "lang", KeyKind::Lang, 481.0, 59.0, 3, 1, true, false, ""));
        l.push(mk("enter", "enter", KeyKind::Enter, 548.0, 84.0, 3, 1, true, true, ""));
        (l, 640.0, 206.0, [4.0, 56.0, 108.0, 160.0], 46.0)
    } else {
        let mut l = Vec::new();
        for (i, &k) in QWERTY[0..10].iter().enumerate() {
            let x = if i == 0 {
                8.0
            } else if i == 9 {
                924.0
            } else {
                108.0 + 102.0 * ((i - 1) as f32)
            };
            let w = if i == 0 || i == 9 { 92.0 } else { 94.0 };
            l.push(mk(k, k, KeyKind::Char, x, w, 0, 1, false, false, ""));
        }
        for (i, &k) in QWERTY[10..19].iter().enumerate() {
            l.push(mk(k, k, KeyKind::Char, 57.0 + 102.0 * (i as f32), 94.0, 1, 1, false, false, ""));
        }
        l.push(sep_key(zh, ja, 8.0, 143.0));
        for (i, &k) in QWERTY[19..26].iter().enumerate() {
            l.push(mk(k, k, KeyKind::Char, 159.0 + 102.0 * (i as f32), 94.0, 2, 1, false, false, ""));
        }
        l.push(mk("del", "del", KeyKind::Del, 873.0, 143.0, 2, 1, true, false, ""));
        if zh {
            l.push(mk("sym", "sym", KeyKind::Sym, 8.0, 143.0, 3, 1, true, false, ""));
            l.push(mk("123", "123", KeyKind::Num, 159.0, 94.0, 3, 1, true, false, ""));
        } else {
            l.push(mk("sym", "sym", KeyKind::Sym, 8.0, 67.0, 3, 1, true, false, ""));
            l.push(mk("123", "123", KeyKind::Num, 83.0, 68.0, 3, 1, true, false, ""));
            l.push(mk("at", "at", KeyKind::Punct, 159.0, 94.0, 3, 1, false, false, "@"));
        }
        l.push(mk("comma", "comma", KeyKind::Punct, 261.0, 94.0, 3, 1, false, false, comma_ch(zh, ja)));
        l.push(mk("space", "space", KeyKind::Space, 363.0, 298.0, 3, 1, false, false, " "));
        l.push(mk("period", "period", KeyKind::Punct, 669.0, 94.0, 3, 1, false, false, period_ch(zh)));
        l.push(mk("lang", "lang", KeyKind::Lang, 771.0, 94.0, 3, 1, true, false, ""));
        l.push(mk("enter", "enter", KeyKind::Enter, 873.0, 143.0, 3, 1, true, true, ""));
        (l, 1024.0, 222.0, [4.0, 60.0, 116.0, 172.0], 50.0)
    };

    let sx = m.kb_w / cw;
    let sy = m.key_h / ch;
    for d in &mut defs {
        let top = m.band_h + ry[d.row] * sy;
        let span_end_row = (d.row + d.row_span - 1).min(3);
        let h = (ry[span_end_row] + kh - ry[d.row]) * sy;
        d.rect = RectF::new(d.x * sx, top, d.w * sx, h);
    }
    defs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_layout_leaves_center_gap_click_through() {
        let m = KbMetrics::compute(KbMode::Split, 1.0, 1682, 1051);
        let keys = build_keys(KbMode::Split, KbLayout::Main, Lang::Zh, &m);
        let regions = m.input_regions(KbMode::Split, true, 0.0);
        assert_eq!(regions.len(), 3, "band + left half + right half");

        let center_x = m.kb_w * 0.5;
        let mid_y = m.band_h + m.key_h * 0.5;
        assert!(
            !regions.iter().any(|r| r.contains(center_x, mid_y)),
            "center gap of split keyboard must be click-through!"
        );
        for k in &keys {
            let (cx, cy) = k.rect.center();
            assert!(
                regions.iter().any(|r| r.contains(cx, cy)),
                "every split key ({}) must sit inside an active input region",
                k.id
            );
        }
    }
}
