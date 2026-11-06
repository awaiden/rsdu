/// Size formatting like gdu: binary (KiB) or SI (kB) prefixes, 1 decimal.
#[derive(Clone, Copy, Default)]
pub struct Fmt {
    pub si: bool,
    pub no_prefix: bool,
    pub kib: bool,
}

impl Fmt {
    pub fn size(&self, n: i64) -> String {
        if self.no_prefix {
            return n.to_string();
        }
        let (base, units): (f64, [&str; 6]) = if self.si {
            (1000.0, ["kB", "MB", "GB", "TB", "PB", "EB"])
        } else {
            (1024.0, ["KiB", "MiB", "GiB", "TiB", "PiB", "EiB"])
        };
        if self.kib {
            return format!("{:.1} {}", n as f64 / base, units[0]);
        }
        let f = n as f64;
        for (i, u) in units.iter().enumerate().rev() {
            let div = base.powi(i as i32 + 1);
            if f.abs() >= div {
                return format!("{:.1} {}", f / div, u);
            }
        }
        format!("{n} B")
    }
}

pub fn count(n: i64) -> String {
    let f = n as f64;
    match n {
        1_000_000_000.. => format!("{:.1}G", f / 1e9),
        1_000_000.. => format!("{:.1}M", f / 1e6),
        1_000.. => format!("{:.1}k", f / 1e3),
        _ => n.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        let f = Fmt::default();
        assert_eq!(f.size(0), "0 B");
        assert_eq!(f.size(1023), "1023 B");
        assert_eq!(f.size(1024), "1.0 KiB");
        assert_eq!(f.size(110592), "108.0 KiB");
        assert_eq!(Fmt { si: true, ..f }.size(1500), "1.5 kB");
        assert_eq!(Fmt { kib: true, ..f }.size(2048), "2.0 KiB");
        assert_eq!(count(1500), "1.5k");
    }
}
