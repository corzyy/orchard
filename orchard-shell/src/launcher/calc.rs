//! Spotlight's calculator and unit converter.
//!
//! `evaluate("2+3*4")` → 14, `convert("10 km in mi")` → 6.2137 mi.
//! Both return `None` for anything that is not clearly a calculation, so
//! typing an app name never shows a bogus result.

/// A finished calculation, ready to show.
#[derive(Debug, Clone, PartialEq)]
pub struct Answer {
    /// what the user typed, tidied up ("2 × 3 + 4")
    pub expr: String,
    /// the result as shown ("10")
    pub value: String,
    /// the result to copy (no grouping separators, '.' decimal point)
    pub plain: String,
    /// "Calculator" or the unit category ("Length")
    pub kind: &'static str,
}

/// Evaluate a math expression. Plain numbers ("42") are not calculations.
pub fn evaluate(input: &str, german: bool) -> Option<Answer> {
    let src = input.trim();
    if src.is_empty() {
        return None;
    }
    let toks = lex(src, german)?;
    // a calculation needs an operator or a function / constant
    let interesting = toks.iter().any(|t| matches!(t, Tok::Op(_) | Tok::Bang))
        || (toks.len() >= 2 && toks.iter().any(|t| matches!(t, Tok::Ident(_))));
    if !interesting {
        return None;
    }
    // a lone leading minus ("-5") is just a number
    if toks.len() == 2 && toks[0] == Tok::Op('-') && matches!(toks[1], Tok::Num(_)) {
        return None;
    }
    let mut p = Parser { toks: &toks, pos: 0 };
    let v = p.expr()?;
    if p.pos != toks.len() || !v.is_finite() {
        return None;
    }
    Some(Answer {
        expr: pretty_expr(&toks, german),
        value: format_number(v, german),
        plain: plain_number(v),
        kind: "Calculator",
    })
}

// ---- lexer -----------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(f64),
    Op(char),
    LParen,
    RParen,
    Bang,
    Ident(String),
}

fn lex(src: &str, german: bool) -> Option<Vec<Tok>> {
    let chars: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c.is_ascii_digit() || c == '.' || (c == ',' && german) {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.' || chars[i] == ',' || chars[i] == '_')
            {
                i += 1;
            }
            let raw: String = chars[start..i].iter().collect();
            out.push(Tok::Num(parse_number(&raw, german)?));
            // scientific notation 1e3
            if i + 1 < chars.len() && (chars[i] == 'e' || chars[i] == 'E') && (chars[i + 1].is_ascii_digit() || chars[i + 1] == '-') {
                let s = i + 1;
                let mut j = s + 1;
                while j < chars.len() && chars[j].is_ascii_digit() {
                    j += 1;
                }
                let exp: String = chars[s..j].iter().collect();
                if let (Ok(e), Some(Tok::Num(n))) = (exp.parse::<i32>(), out.last_mut()) {
                    *n *= 10f64.powi(e);
                    i = j;
                }
            }
            continue;
        }
        // "x" between numbers is multiplication: 3x4, 2 x (1+1)
        if (c == 'x' || c == 'X')
            && matches!(out.last(), Some(Tok::Num(_)) | Some(Tok::RParen))
            && chars.get(i + 1).is_some_and(|n| n.is_ascii_digit() || n.is_whitespace() || *n == '(' || *n == '.')
        {
            out.push(Tok::Op('*'));
            i += 1;
            continue;
        }
        if c.is_alphabetic() || c == 'π' || c == '√' {
            if c == 'π' || c == '√' {
                out.push(Tok::Ident(if c == 'π' { "pi".into() } else { "sqrt".into() }));
                i += 1;
                continue;
            }
            let start = i;
            while i < chars.len() && (chars[i].is_alphanumeric()) {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect::<String>().to_lowercase();
            // "x" between numbers is multiplication: 3x4
            if word == "x" && matches!(out.last(), Some(Tok::Num(_)) | Some(Tok::RParen)) {
                out.push(Tok::Op('*'));
                continue;
            }
            if word == "mod" {
                out.push(Tok::Op('%'));
                continue;
            }
            out.push(Tok::Ident(word));
            continue;
        }
        let t = match c {
            '+' => Tok::Op('+'),
            '-' | '−' | '–' => Tok::Op('-'),
            '*' | '×' | '·' | '⋅' => Tok::Op('*'),
            '/' | '÷' | ':' => Tok::Op('/'),
            '%' => Tok::Op('%'),
            '^' => Tok::Op('^'),
            '(' | '[' => Tok::LParen,
            ')' | ']' => Tok::RParen,
            '!' => Tok::Bang,
            '²' => {
                out.push(Tok::Op('^'));
                Tok::Num(2.0)
            }
            '³' => {
                out.push(Tok::Op('^'));
                Tok::Num(3.0)
            }
            _ => return None,
        };
        // "**" is power
        if t == Tok::Op('*') && out.last() == Some(&Tok::Op('*')) {
            out.pop();
            out.push(Tok::Op('^'));
        } else {
            out.push(t);
        }
        i += 1;
    }
    Some(out)
}

/// "1,234.5", "1.234,5" (German), "1_000", "3,5" → f64
fn parse_number(raw: &str, german: bool) -> Option<f64> {
    let raw = raw.replace('_', "");
    let three = |sep: char| {
        let mut parts = raw.split(sep);
        let first = parts.next().unwrap_or_default();
        !first.is_empty() && first.len() <= 3 && raw.contains(sep) && parts.all(|g| g.len() == 3)
    };
    let s = if german {
        if raw.contains(',') {
            raw.replace('.', "").replace(',', ".")
        } else if three('.') {
            raw.replace('.', "")
        } else {
            raw
        }
    } else if three(',') {
        raw.replace(',', "")
    } else {
        raw.replace(',', ".")
    };
    if s.matches('.').count() > 1 {
        return None;
    }
    s.parse().ok()
}

// ---- parser (precedence climbing) -------------------------------------------

struct Parser<'a> {
    toks: &'a [Tok],
    pos: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }
    fn next(&mut self) -> Option<&Tok> {
        let t = self.toks.get(self.pos);
        self.pos += 1;
        t
    }

    // expr := term (('+'|'-') term)*
    fn expr(&mut self) -> Option<f64> {
        let mut v = self.term()?;
        while let Some(Tok::Op(op @ ('+' | '-'))) = self.peek().cloned() {
            self.pos += 1;
            let rhs = self.term()?;
            // "200 + 10%" adds ten percent, as on a pocket calculator
            let rhs = if self.prev_was_percent() { v * rhs } else { rhs };
            v = if op == '+' { v + rhs } else { v - rhs };
        }
        Some(v)
    }

    fn prev_was_percent(&self) -> bool {
        self.pos >= 1 && self.toks.get(self.pos - 1) == Some(&Tok::Op('%')) && self.percent_postfix(self.pos - 1)
    }

    /// '%' at `i` is a postfix percent (not modulo) when nothing that could be
    /// an operand follows it.
    fn percent_postfix(&self, i: usize) -> bool {
        !matches!(self.toks.get(i + 1), Some(Tok::Num(_) | Tok::LParen | Tok::Ident(_)))
    }

    // term := unary (('*'|'/'|'%') unary | implicit-mul)*
    fn term(&mut self) -> Option<f64> {
        let mut v = self.unary()?;
        loop {
            match self.peek().cloned() {
                Some(Tok::Op(op @ ('*' | '/'))) => {
                    self.pos += 1;
                    let rhs = self.unary()?;
                    v = if op == '*' { v * rhs } else { v / rhs };
                }
                Some(Tok::Op('%')) if !self.percent_postfix(self.pos) => {
                    self.pos += 1;
                    let rhs = self.unary()?;
                    v %= rhs;
                }
                // 2pi, 3(4+1), 2 sqrt 9
                Some(Tok::LParen | Tok::Ident(_)) => {
                    let rhs = self.unary()?;
                    v *= rhs;
                }
                _ => return Some(v),
            }
        }
    }

    // unary := '-' unary | power
    fn unary(&mut self) -> Option<f64> {
        match self.peek() {
            Some(Tok::Op('-')) => {
                self.pos += 1;
                Some(-self.unary()?)
            }
            Some(Tok::Op('+')) => {
                self.pos += 1;
                self.unary()
            }
            _ => self.power(),
        }
    }

    // power := postfix ('^' unary)?   (right associative)
    fn power(&mut self) -> Option<f64> {
        let base = self.postfix()?;
        if self.peek() == Some(&Tok::Op('^')) {
            self.pos += 1;
            let e = self.unary()?;
            return Some(base.powf(e));
        }
        Some(base)
    }

    // postfix := atom ('!' | '%')*
    fn postfix(&mut self) -> Option<f64> {
        let mut v = self.atom()?;
        loop {
            match self.peek() {
                Some(Tok::Bang) => {
                    self.pos += 1;
                    v = factorial(v)?;
                }
                Some(Tok::Op('%')) if self.percent_postfix(self.pos) => {
                    self.pos += 1;
                    v /= 100.0;
                }
                _ => return Some(v),
            }
        }
    }

    fn atom(&mut self) -> Option<f64> {
        match self.next()?.clone() {
            Tok::Num(n) => Some(n),
            Tok::LParen => {
                let v = self.expr()?;
                // tolerate a missing closing parenthesis at the very end: "(2+3"
                match self.peek() {
                    Some(Tok::RParen) => self.pos += 1,
                    None => {}
                    _ => return None,
                }
                Some(v)
            }
            Tok::Ident(name) => match name.as_str() {
                "pi" => Some(std::f64::consts::PI),
                "e" => Some(std::f64::consts::E),
                "tau" => Some(std::f64::consts::TAU),
                "phi" => Some(1.618_033_988_749_895),
                _ => {
                    let f: fn(f64) -> f64 = match name.as_str() {
                        "sqrt" => f64::sqrt,
                        "cbrt" => f64::cbrt,
                        "sin" => |x: f64| x.sin(),
                        "cos" => |x: f64| x.cos(),
                        "tan" => |x: f64| x.tan(),
                        "asin" | "arcsin" => f64::asin,
                        "acos" | "arccos" => f64::acos,
                        "atan" | "arctan" => f64::atan,
                        "sinh" => f64::sinh,
                        "cosh" => f64::cosh,
                        "tanh" => f64::tanh,
                        "ln" => f64::ln,
                        "log" | "lg" => f64::log10,
                        "log2" | "lb" => f64::log2,
                        "exp" => f64::exp,
                        "abs" => f64::abs,
                        "floor" => f64::floor,
                        "ceil" => f64::ceil,
                        "round" => f64::round,
                        _ => return None,
                    };
                    // function argument: parenthesised or a single factor ("sqrt 16")
                    let arg = self.power()?;
                    Some(f(arg))
                }
            },
            _ => None,
        }
    }
}

fn factorial(v: f64) -> Option<f64> {
    if v < 0.0 || v.fract() != 0.0 || v > 170.0 {
        return None;
    }
    Some((1..=v as u64).map(|i| i as f64).product())
}

// ---- formatting --------------------------------------------------------------

/// The result as macOS shows it: up to 10 significant digits, grouped.
pub fn format_number(v: f64, german: bool) -> String {
    let (dec, group) = if german { (',', '.') } else { ('.', ',') };
    if v == 0.0 {
        return "0".into();
    }
    let a = v.abs();
    if !(1e-6..1e15).contains(&a) {
        // scientific: 1.2345e20
        let s = format!("{:.6e}", v);
        let (m, e) = s.split_once('e').unwrap_or((&s, "0"));
        let m = m.trim_end_matches('0').trim_end_matches('.');
        return format!("{}e{}", m.replace('.', &dec.to_string()), e);
    }
    let digits = (10 - (a.log10().floor() as i32 + 1)).clamp(0, 10) as usize;
    let s = format!("{:.*}", digits, a);
    let s = if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s };
    let (int, frac) = s.split_once('.').map(|(i, f)| (i.to_string(), Some(f.to_string()))).unwrap_or((s.clone(), None));
    let mut grouped = String::new();
    for (i, c) in int.chars().enumerate() {
        if i > 0 && (int.len() - i) % 3 == 0 {
            grouped.push(group);
        }
        grouped.push(c);
    }
    let mut out = if v < 0.0 { format!("−{grouped}") } else { grouped };
    if let Some(f) = frac {
        out.push(dec);
        out.push_str(&f);
    }
    out
}

/// The result for the clipboard: no grouping, '.' as decimal point.
pub fn plain_number(v: f64) -> String {
    let s = format!("{:.10}", v);
    let s = if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s };
    if s == "-0" {
        "0".into()
    } else {
        s
    }
}

fn pretty_expr(toks: &[Tok], german: bool) -> String {
    let mut out = String::new();
    for (i, t) in toks.iter().enumerate() {
        let piece = match t {
            Tok::Num(n) => format_number(*n, german).replace('−', "-"),
            Tok::Op('*') => " × ".into(),
            Tok::Op('/') => " ÷ ".into(),
            Tok::Op('-') if i == 0 || matches!(toks[i - 1], Tok::Op(_) | Tok::LParen) => "−".into(),
            Tok::Op('-') => " − ".into(),
            Tok::Op('+') => " + ".into(),
            Tok::Op('^') => "^".into(),
            Tok::Op('%') => "%".into(),
            Tok::Op(c) => format!(" {c} "),
            Tok::LParen => "(".into(),
            Tok::RParen => ")".into(),
            Tok::Bang => "!".into(),
            Tok::Ident(s) => match s.as_str() {
                "pi" => "π".into(),
                "sqrt" => "√".into(),
                s => s.to_string(),
            },
        };
        out.push_str(&piece);
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ---- unit conversion -----------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Dim {
    Length,
    Mass,
    Volume,
    Temperature,
    Data,
    Time,
    Speed,
    Area,
    Energy,
}

impl Dim {
    fn name(self, german: bool) -> &'static str {
        match (self, german) {
            (Dim::Length, false) => "Length",
            (Dim::Length, true) => "Länge",
            (Dim::Mass, false) => "Mass",
            (Dim::Mass, true) => "Masse",
            (Dim::Volume, false) => "Volume",
            (Dim::Volume, true) => "Volumen",
            (Dim::Temperature, false) => "Temperature",
            (Dim::Temperature, true) => "Temperatur",
            (Dim::Data, false) => "Data Size",
            (Dim::Data, true) => "Datengröße",
            (Dim::Time, false) => "Time",
            (Dim::Time, true) => "Zeit",
            (Dim::Speed, false) => "Speed",
            (Dim::Speed, true) => "Geschwindigkeit",
            (Dim::Area, false) => "Area",
            (Dim::Area, true) => "Fläche",
            (Dim::Energy, false) => "Energy",
            (Dim::Energy, true) => "Energie",
        }
    }
}

struct Unit {
    names: &'static [&'static str],
    /// shown after the number
    symbol: &'static str,
    dim: Dim,
    /// value in the base unit = x * factor (temperatures are special-cased)
    factor: f64,
}

const UNITS: &[Unit] = &[
    // length, base metre
    Unit { names: &["mm", "millimeter", "millimeters", "millimetre", "millimetres", "millimeter"], symbol: "mm", dim: Dim::Length, factor: 0.001 },
    Unit { names: &["cm", "centimeter", "centimeters", "centimetre", "centimetres", "zentimeter"], symbol: "cm", dim: Dim::Length, factor: 0.01 },
    Unit { names: &["m", "meter", "meters", "metre", "metres"], symbol: "m", dim: Dim::Length, factor: 1.0 },
    Unit { names: &["km", "kilometer", "kilometers", "kilometre", "kilometres"], symbol: "km", dim: Dim::Length, factor: 1000.0 },
    Unit { names: &["in", "inch", "inches", "zoll", "\""], symbol: "in", dim: Dim::Length, factor: 0.0254 },
    Unit { names: &["ft", "foot", "feet", "fuß", "fuss", "'"], symbol: "ft", dim: Dim::Length, factor: 0.3048 },
    Unit { names: &["yd", "yard", "yards"], symbol: "yd", dim: Dim::Length, factor: 0.9144 },
    Unit { names: &["mi", "mile", "miles", "meile", "meilen"], symbol: "mi", dim: Dim::Length, factor: 1609.344 },
    Unit { names: &["nmi", "nautical mile", "nautical miles", "seemeile", "seemeilen"], symbol: "nmi", dim: Dim::Length, factor: 1852.0 },
    // mass, base kilogram
    Unit { names: &["mg", "milligram", "milligrams", "milligramm"], symbol: "mg", dim: Dim::Mass, factor: 1e-6 },
    Unit { names: &["g", "gram", "grams", "gramm"], symbol: "g", dim: Dim::Mass, factor: 0.001 },
    Unit { names: &["kg", "kilo", "kilos", "kilogram", "kilograms", "kilogramm"], symbol: "kg", dim: Dim::Mass, factor: 1.0 },
    Unit { names: &["t", "ton", "tons", "tonne", "tonnes", "tonnen"], symbol: "t", dim: Dim::Mass, factor: 1000.0 },
    Unit { names: &["oz", "ounce", "ounces", "unze", "unzen"], symbol: "oz", dim: Dim::Mass, factor: 0.028_349_523_125 },
    Unit { names: &["lb", "lbs", "pound", "pounds", "pfund"], symbol: "lb", dim: Dim::Mass, factor: 0.453_592_37 },
    Unit { names: &["st", "stone", "stones"], symbol: "st", dim: Dim::Mass, factor: 6.350_293_18 },
    // volume, base litre
    Unit { names: &["ml", "milliliter", "milliliters", "millilitre", "millilitres"], symbol: "ml", dim: Dim::Volume, factor: 0.001 },
    Unit { names: &["cl", "centiliter", "centiliters"], symbol: "cl", dim: Dim::Volume, factor: 0.01 },
    Unit { names: &["dl", "deciliter", "deciliters"], symbol: "dl", dim: Dim::Volume, factor: 0.1 },
    Unit { names: &["l", "liter", "liters", "litre", "litres"], symbol: "l", dim: Dim::Volume, factor: 1.0 },
    Unit { names: &["m3", "m³", "cubic meter", "cubic meters", "kubikmeter"], symbol: "m³", dim: Dim::Volume, factor: 1000.0 },
    Unit { names: &["gal", "gallon", "gallons", "gallone", "gallonen"], symbol: "gal", dim: Dim::Volume, factor: 3.785_411_784 },
    Unit { names: &["qt", "quart", "quarts"], symbol: "qt", dim: Dim::Volume, factor: 0.946_352_946 },
    Unit { names: &["pt", "pint", "pints"], symbol: "pt", dim: Dim::Volume, factor: 0.473_176_473 },
    Unit { names: &["cup", "cups", "tasse", "tassen"], symbol: "cups", dim: Dim::Volume, factor: 0.236_588_236_5 },
    Unit { names: &["floz", "fl oz", "fluid ounce", "fluid ounces"], symbol: "fl oz", dim: Dim::Volume, factor: 0.029_573_529_6 },
    Unit { names: &["tbsp", "tablespoon", "tablespoons", "el", "esslöffel"], symbol: "tbsp", dim: Dim::Volume, factor: 0.014_786_764_8 },
    Unit { names: &["tsp", "teaspoon", "teaspoons", "tl", "teelöffel"], symbol: "tsp", dim: Dim::Volume, factor: 0.004_928_921_59 },
    // temperature (factor unused)
    Unit { names: &["c", "°c", "celsius", "degc"], symbol: "°C", dim: Dim::Temperature, factor: 0.0 },
    Unit { names: &["f", "°f", "fahrenheit", "degf"], symbol: "°F", dim: Dim::Temperature, factor: 0.0 },
    Unit { names: &["k", "kelvin"], symbol: "K", dim: Dim::Temperature, factor: 0.0 },
    // data, base byte
    Unit { names: &["bit", "bits"], symbol: "bit", dim: Dim::Data, factor: 0.125 },
    Unit { names: &["b", "byte", "bytes"], symbol: "B", dim: Dim::Data, factor: 1.0 },
    Unit { names: &["kb", "kilobyte", "kilobytes"], symbol: "KB", dim: Dim::Data, factor: 1e3 },
    Unit { names: &["mb", "megabyte", "megabytes"], symbol: "MB", dim: Dim::Data, factor: 1e6 },
    Unit { names: &["gb", "gigabyte", "gigabytes"], symbol: "GB", dim: Dim::Data, factor: 1e9 },
    Unit { names: &["tb", "terabyte", "terabytes"], symbol: "TB", dim: Dim::Data, factor: 1e12 },
    Unit { names: &["kib", "kibibyte", "kibibytes"], symbol: "KiB", dim: Dim::Data, factor: 1024.0 },
    Unit { names: &["mib", "mebibyte", "mebibytes"], symbol: "MiB", dim: Dim::Data, factor: 1_048_576.0 },
    Unit { names: &["gib", "gibibyte", "gibibytes"], symbol: "GiB", dim: Dim::Data, factor: 1_073_741_824.0 },
    Unit { names: &["tib", "tebibyte", "tebibytes"], symbol: "TiB", dim: Dim::Data, factor: 1_099_511_627_776.0 },
    // time, base second
    Unit { names: &["ms", "millisecond", "milliseconds", "millisekunde", "millisekunden"], symbol: "ms", dim: Dim::Time, factor: 0.001 },
    Unit { names: &["s", "sec", "secs", "second", "seconds", "sekunde", "sekunden"], symbol: "s", dim: Dim::Time, factor: 1.0 },
    Unit { names: &["min", "mins", "minute", "minutes", "minuten"], symbol: "min", dim: Dim::Time, factor: 60.0 },
    Unit { names: &["h", "hr", "hrs", "hour", "hours", "std", "stunde", "stunden"], symbol: "h", dim: Dim::Time, factor: 3600.0 },
    Unit { names: &["d", "day", "days", "tag", "tage"], symbol: "days", dim: Dim::Time, factor: 86_400.0 },
    Unit { names: &["wk", "week", "weeks", "woche", "wochen"], symbol: "weeks", dim: Dim::Time, factor: 604_800.0 },
    Unit { names: &["yr", "year", "years", "jahr", "jahre"], symbol: "years", dim: Dim::Time, factor: 31_557_600.0 },
    // speed, base m/s
    Unit { names: &["m/s", "mps"], symbol: "m/s", dim: Dim::Speed, factor: 1.0 },
    Unit { names: &["km/h", "kmh", "kph", "kmph"], symbol: "km/h", dim: Dim::Speed, factor: 1.0 / 3.6 },
    Unit { names: &["mph", "mi/h"], symbol: "mph", dim: Dim::Speed, factor: 0.447_04 },
    Unit { names: &["kn", "kt", "knot", "knots", "knoten"], symbol: "kn", dim: Dim::Speed, factor: 0.514_444 },
    // area, base m²
    Unit { names: &["m2", "m²", "sqm", "square meter", "square meters", "quadratmeter"], symbol: "m²", dim: Dim::Area, factor: 1.0 },
    Unit { names: &["km2", "km²", "square kilometer", "square kilometers", "quadratkilometer"], symbol: "km²", dim: Dim::Area, factor: 1e6 },
    Unit { names: &["ft2", "ft²", "sqft", "square foot", "square feet"], symbol: "ft²", dim: Dim::Area, factor: 0.092_903_04 },
    Unit { names: &["ha", "hectare", "hectares", "hektar"], symbol: "ha", dim: Dim::Area, factor: 10_000.0 },
    Unit { names: &["ac", "acre", "acres"], symbol: "ac", dim: Dim::Area, factor: 4046.856_422_4 },
    // energy, base joule
    Unit { names: &["j", "joule", "joules"], symbol: "J", dim: Dim::Energy, factor: 1.0 },
    Unit { names: &["kj", "kilojoule", "kilojoules"], symbol: "kJ", dim: Dim::Energy, factor: 1000.0 },
    Unit { names: &["cal", "calorie", "calories", "kalorie", "kalorien"], symbol: "cal", dim: Dim::Energy, factor: 4.184 },
    Unit { names: &["kcal", "kilocalorie", "kilocalories", "kilokalorie", "kilokalorien"], symbol: "kcal", dim: Dim::Energy, factor: 4184.0 },
    Unit { names: &["wh", "watt hour", "watt hours"], symbol: "Wh", dim: Dim::Energy, factor: 3600.0 },
    Unit { names: &["kwh", "kilowatt hour", "kilowatt hours"], symbol: "kWh", dim: Dim::Energy, factor: 3.6e6 },
];

fn unit(name: &str) -> Option<&'static Unit> {
    let n = name.trim().to_lowercase();
    let n = n.trim_start_matches("degrees ").trim_start_matches("degree ").trim_start_matches("grad ");
    UNITS.iter().find(|u| u.names.contains(&n))
}

/// The unit people usually want when they type a value without a target:
/// metric ↔ imperial.
fn default_target(u: &Unit) -> Option<&'static Unit> {
    let target = match u.symbol {
        "mm" | "cm" => "in",
        "m" => "ft",
        "km" => "mi",
        "in" => "cm",
        "ft" => "m",
        "yd" => "m",
        "mi" | "nmi" => "km",
        "mg" | "g" => "oz",
        "kg" => "lb",
        "t" => "lb",
        "oz" => "g",
        "lb" | "st" => "kg",
        "ml" | "cl" | "dl" => "floz",
        "l" => "gal",
        "m³" => "gal",
        "gal" | "qt" | "pt" | "cups" | "fl oz" | "tbsp" | "tsp" => "ml",
        "°C" => "f",
        "°F" => "c",
        "K" => "c",
        "MB" => "mib",
        "GB" => "gib",
        "TB" => "tib",
        "KB" => "kib",
        "KiB" => "kb",
        "MiB" => "mb",
        "GiB" => "gb",
        "TiB" => "tb",
        "km/h" => "mph",
        "mph" => "km/h",
        "m/s" => "km/h",
        "kn" => "km/h",
        "m²" => "ft2",
        "ft²" => "m2",
        "ha" => "ac",
        "ac" => "ha",
        "km²" => "mi",
        "kcal" => "kj",
        "kJ" => "kcal",
        "cal" => "j",
        "J" => "cal",
        "kWh" => "kj",
        "min" => "s",
        "h" => "min",
        "days" => "h",
        "weeks" => "days",
        "years" => "days",
        _ => return None,
    };
    unit(target).filter(|t| t.dim == u.dim)
}

fn to_base(u: &Unit, v: f64) -> f64 {
    match u.symbol {
        "°C" => v + 273.15,
        "°F" => (v - 32.0) * 5.0 / 9.0 + 273.15,
        "K" => v,
        _ => v * u.factor,
    }
}

fn from_base(u: &Unit, v: f64) -> f64 {
    match u.symbol {
        "°C" => v - 273.15,
        "°F" => (v - 273.15) * 9.0 / 5.0 + 32.0,
        "K" => v,
        _ => v / u.factor,
    }
}

/// "10 km in mi", "100f to c", "5 kg", "3.5 GB in MiB" → converted value.
pub fn convert(input: &str, german: bool) -> Option<Answer> {
    let s = input.trim().to_lowercase();
    // split off the target unit: " in ", " to ", " into ", " → ", " -> ", " = ", " nach ", " zu "
    let (lhs, rhs) = [" in ", " to ", " into ", " -> ", " → ", " = ", " nach ", " zu ", " as "]
        .iter()
        .find_map(|sep| s.rsplit_once(sep))
        .map(|(a, b)| (a.trim().to_string(), Some(b.trim().to_string())))
        .unwrap_or((s.clone(), None));
    // the value: a number (or calculation) followed by a unit
    let split = lhs
        .char_indices()
        .find(|(_, c)| c.is_alphabetic() || *c == '°' || *c == '"' || *c == '\'')
        .map(|(i, _)| i)?;
    let (num, uname) = lhs.split_at(split);
    let num = num.trim();
    if num.is_empty() {
        return None;
    }
    let value = match parse_number(num, german) {
        Some(v) => v,
        None => {
            let toks = lex(num, german)?;
            let mut p = Parser { toks: &toks, pos: 0 };
            let v = p.expr()?;
            if p.pos != toks.len() {
                return None;
            }
            v
        }
    };
    let from = unit(uname)?;
    let to = match rhs {
        Some(r) => unit(&r)?,
        None => default_target(from)?,
    };
    if from.dim != to.dim || std::ptr::eq(from, to) {
        return None;
    }
    let out = from_base(to, to_base(from, value));
    if !out.is_finite() {
        return None;
    }
    let sym = |u: &Unit| if u.symbol.starts_with('°') { u.symbol.to_string() } else { format!(" {}", u.symbol) };
    Some(Answer {
        expr: format!("{}{}", format_number(value, german), sym(from)),
        value: format!("{}{}", format_number(out, german), sym(to)),
        plain: plain_number(out),
        kind: from.dim.name(german),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(s: &str) -> Option<String> {
        evaluate(s, false).map(|a| a.plain)
    }

    #[test]
    fn arithmetic() {
        assert_eq!(ev("2+3*4").as_deref(), Some("14"));
        assert_eq!(ev("(2+3)*4").as_deref(), Some("20"));
        assert_eq!(ev("2^10").as_deref(), Some("1024"));
        assert_eq!(ev("2^3^2").as_deref(), Some("512"));
        assert_eq!(ev("-2^2").as_deref(), Some("-4"));
        assert_eq!(ev("10/4").as_deref(), Some("2.5"));
        assert_eq!(ev("7 mod 3").as_deref(), Some("1"));
        assert_eq!(ev("5!").as_deref(), Some("120"));
        assert_eq!(ev("sqrt(16)+1").as_deref(), Some("5"));
        assert_eq!(ev("2pi").map(|s| s[..4].to_string()).as_deref(), Some("6.28"));
        assert_eq!(ev("3x4").as_deref(), Some("12"));
        assert_eq!(ev("200+10%").as_deref(), Some("220"));
        assert_eq!(ev("50%").as_deref(), Some("0.5"));
        assert_eq!(ev("1,000*2").as_deref(), Some("2000"));
        assert_eq!(ev("(1+2").as_deref(), Some("3"));
        assert_eq!(ev("0.1+0.2").as_deref(), Some("0.3"));
    }

    #[test]
    fn not_calculations() {
        assert_eq!(ev("firefox"), None);
        assert_eq!(ev("42"), None);
        assert_eq!(ev("-5"), None);
        assert_eq!(ev("1/0"), None);
        assert_eq!(ev("2+"), None);
        assert_eq!(ev("kate"), None);
        assert_eq!(ev("e"), None);
        assert_eq!(ev("pi"), None);
        assert!(ev("sqrt 2").is_some());
    }

    #[test]
    fn german_numbers() {
        assert_eq!(evaluate("3,5*2", true).map(|a| a.plain).as_deref(), Some("7"));
        assert_eq!(evaluate("1.000+1", true).map(|a| a.plain).as_deref(), Some("1001"));
        assert_eq!(format_number(1234.5, true), "1.234,5");
        assert_eq!(format_number(1234.5, false), "1,234.5");
    }

    #[test]
    fn conversions() {
        let c = |s: &str| convert(s, false).map(|a| a.value);
        assert_eq!(c("10 km in mi").as_deref(), Some("6.213711922 mi"));
        assert_eq!(c("100 f to c").as_deref(), Some("37.77777778°C"));
        assert_eq!(c("0c").as_deref(), Some("32°F"));
        assert_eq!(c("1 gib in mb").as_deref(), Some("1,073.741824 MB"));
        assert_eq!(c("2 kg").as_deref(), Some("4.409245244 lb"));
        assert_eq!(c("firefox"), None);
        assert_eq!(c("10 km in kg"), None);
        assert_eq!(c("km"), None);
    }

    #[test]
    fn formatting() {
        assert_eq!(format_number(1e20, false), "1e20");
        assert_eq!(format_number(-1234.0, false), "−1,234");
        assert_eq!(format_number(1.0 / 3.0, false), "0.3333333333");
    }
}
