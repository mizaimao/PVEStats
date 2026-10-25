//! Number, duration and text formatting. Spec §7: round everything on screen.

pub const MIB: f64 = 1024.0 * 1024.0;
pub const GIB: f64 = MIB * 1024.0;
pub const TIB: f64 = GIB * 1024.0;

pub const DASH: &str = "–";

pub fn gib(v: Option<f64>) -> String {
    v.map_or_else(|| DASH.into(), |v| format!("{:.1}", v / GIB))
}


/// Bytes per second as MiB/s, one decimal.
pub fn rate(v: Option<f64>) -> String {
    v.map_or_else(|| DASH.into(), |v| format!("{:.1}", v / MIB))
}

/// Bytes per second in the unit that suits it, as btop prints rates.
pub fn rate_auto(v: f64) -> String {
    if v >= GIB {
        format!("{:.1} GiB/s", v / GIB)
    } else if v >= MIB {
        format!("{:.1} MiB/s", v / MIB)
    } else {
        format!("{:.0} KiB/s", v / 1024.0)
    }
}

/// A percentage with its sign, or a bare dash when missing.
pub fn pct(v: Option<f64>) -> String {
    v.map_or_else(|| DASH.into(), |v| format!("{}%", v.round() as i64))
}

pub fn uptime(seconds: Option<f64>) -> String {
    let Some(s) = seconds else { return DASH.into() };
    let s = s.max(0.0) as u64;
    let (d, h, m) = (s / 86_400, (s % 86_400) / 3600, (s % 3600) / 60);
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else {
        format!("{m}m")
    }
}

/// Time until a model unloads. Ollama moves a model's expiry only when a
/// request finishes, so one still listed well past it is busy; keep_alive -1
/// puts the expiry centuries out.
pub fn countdown(seconds: Option<i64>) -> String {
    let Some(s) = seconds else { return DASH.into() };
    if s > 20 * 365 * 86_400 {
        return "never".into();
    }
    if s < -40 {
        return "in use".into();
    }
    if s <= 0 {
        return "now".into();
    }
    let (h, m) = (s / 3600, (s % 3600) / 60);
    if h > 0 {
        format!("{h}h {m}m")
    } else if m > 0 {
        format!("{m}m")
    } else {
        format!("{s}s")
    }
}

/// Thousands separators, as the app prints the SM clock.
pub fn int(v: Option<f64>) -> String {
    let Some(v) = v else { return DASH.into() };
    let digits = (v.round() as i64).to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 && c != '-' {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Cut to `width` characters, marking the cut with an ellipsis.
pub fn fit(s: &str, width: usize) -> String {
    let n = s.chars().count();
    if n <= width {
        return s.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let mut out: String = s.chars().take(width - 1).collect();
    out.push('…');
    out
}

pub fn left(s: &str, width: usize) -> String {
    let s = fit(s, width);
    let pad = width.saturating_sub(s.chars().count());
    format!("{s}{}", " ".repeat(pad))
}

pub fn right(s: &str, width: usize) -> String {
    let s = fit(s, width);
    let pad = width.saturating_sub(s.chars().count());
    format!("{}{s}", " ".repeat(pad))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounding_and_dashes() {
        assert_eq!(gib(Some(41.0 * GIB)), "41.0");
        assert_eq!(gib(None), "–");
        assert_eq!(pct(Some(63.4)), "63%");
        assert_eq!(pct(None), "–");
        assert_eq!(rate(Some(1.5 * MIB)), "1.5");
        assert_eq!(rate_auto(1.5 * MIB), "1.5 MiB/s");
        assert_eq!(rate_auto(13_000.0), "13 KiB/s");
        assert_eq!(int(Some(2100.0)), "2,100");
        assert_eq!(int(Some(210.0)), "210");
    }

    #[test]
    fn durations() {
        assert_eq!(uptime(Some(500_000.0)), "5d 18h");
        assert_eq!(uptime(Some(3_720.0)), "1h 2m");
        assert_eq!(uptime(Some(420.0)), "7m");
        assert_eq!(countdown(Some(9_200_000_000)), "never");
        assert_eq!(countdown(Some(-90)), "in use");
        assert_eq!(countdown(Some(-10)), "now");
        assert_eq!(countdown(Some(300)), "5m");
        assert_eq!(countdown(Some(45)), "45s");
    }

    #[test]
    fn fitting() {
        assert_eq!(fit("CompoundZ2FS", 8), "Compoun…");
        assert_eq!(left("ab", 4), "ab  ");
        assert_eq!(right("ab", 4), "  ab");
    }
}
