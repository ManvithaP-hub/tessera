//! Parsing for Kubernetes resource quantities ("500m", "1.5", "256Mi", "1G").

/// CPU quantity to millicores.
pub fn cpu_milli(q: &str) -> Option<i64> {
    let q = q.trim();
    if let Some(m) = q.strip_suffix('m') {
        return m.parse::<f64>().ok().map(|v| v.round() as i64);
    }
    if let Some(n) = q.strip_suffix('n') {
        return n.parse::<f64>().ok().map(|v| (v / 1_000_000.0).round() as i64);
    }
    if let Some(u) = q.strip_suffix('u') {
        return u.parse::<f64>().ok().map(|v| (v / 1_000.0).round() as i64);
    }
    q.parse::<f64>().ok().map(|v| (v * 1000.0).round() as i64)
}

/// Memory quantity to bytes.
pub fn bytes(q: &str) -> Option<i64> {
    let q = q.trim();
    const UNITS: [(&str, f64); 12] = [
        ("Ki", 1024.0),
        ("Mi", 1048576.0),
        ("Gi", 1073741824.0),
        ("Ti", 1099511627776.0),
        ("Pi", 1125899906842624.0),
        ("Ei", 1152921504606846976.0),
        ("k", 1e3),
        ("M", 1e6),
        ("G", 1e9),
        ("T", 1e12),
        ("P", 1e15),
        ("E", 1e18),
    ];
    for (suffix, mult) in UNITS {
        if let Some(n) = q.strip_suffix(suffix) {
            return n.parse::<f64>().ok().map(|v| (v * mult).round() as i64);
        }
    }
    if let Some(m) = q.strip_suffix('m') {
        return m.parse::<f64>().ok().map(|v| (v / 1000.0).round() as i64);
    }
    // Plain integers and exponent forms such as 129e6.
    q.parse::<f64>().ok().map(|v| v.round() as i64)
}

/// Human readable binary size, e.g. 268435456 -> "256Mi".
pub fn fmt_bytes(b: i64) -> String {
    let units = [("Gi", 1i64 << 30), ("Mi", 1 << 20), ("Ki", 1 << 10)];
    for (u, size) in units {
        if b >= size {
            let v = b as f64 / size as f64;
            return if (v - v.round()).abs() < 0.05 {
                format!("{}{u}", v.round() as i64)
            } else {
                format!("{v:.1}{u}")
            };
        }
    }
    format!("{b}B")
}

/// Human readable CPU, e.g. 500 -> "500m", 2000 -> "2".
pub fn fmt_cpu(milli: i64) -> String {
    if milli % 1000 == 0 {
        format!("{}", milli / 1000)
    } else if milli > 1000 {
        format!("{:.2}", milli as f64 / 1000.0)
    } else {
        format!("{milli}m")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu() {
        assert_eq!(cpu_milli("500m"), Some(500));
        assert_eq!(cpu_milli("2"), Some(2000));
        assert_eq!(cpu_milli("0.25"), Some(250));
        assert_eq!(cpu_milli("3920m"), Some(3920));
        assert_eq!(cpu_milli("250000000n"), Some(250));
        assert_eq!(cpu_milli("abc"), None);
    }

    #[test]
    fn memory() {
        assert_eq!(bytes("256Mi"), Some(268435456));
        assert_eq!(bytes("1Gi"), Some(1073741824));
        assert_eq!(bytes("512M"), Some(512_000_000));
        assert_eq!(bytes("129e6"), Some(129_000_000));
        assert_eq!(bytes("128974848"), Some(128974848));
        assert_eq!(bytes("15564212Ki"), Some(15564212 * 1024));
    }

    #[test]
    fn formatting() {
        assert_eq!(fmt_bytes(268435456), "256Mi");
        assert_eq!(fmt_bytes(1610612736), "1.5Gi");
        assert_eq!(fmt_cpu(500), "500m");
        assert_eq!(fmt_cpu(6000), "6");
        assert_eq!(fmt_cpu(3920), "3.92");
    }
}
