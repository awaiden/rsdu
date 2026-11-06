use crate::tree::{EMPTY_DIR_SIZE, Item, SmallStr, fold_dev};
use std::io::{self, Write};

/// `--output-attrs` selection; empty means everything.
#[derive(Default, Clone)]
pub struct Attrs(pub Vec<String>);

#[derive(Clone, Copy)]
struct AttrFlags {
    asize: bool,
    dsize: bool,
    mtime: bool,
    notreg: bool,
    ino: bool,
}

impl Attrs {
    fn has(&self, a: &str) -> bool {
        self.0.is_empty() || self.0.iter().any(|x| x == a)
    }

    fn flags(&self) -> AttrFlags {
        AttrFlags {
            asize: self.has("asize"),
            dsize: self.has("dsize"),
            mtime: self.has("mtime"),
            notreg: self.has("notreg"),
            ino: self.has("ino"),
        }
    }
}

#[inline]
fn write_json_str(w: &mut impl Write, s: &str) -> io::Result<()> {
    if !s.bytes().any(|b| b < 0x20 || b == b'"' || b == b'\\') {
        w.write_all(b"\"")?;
        w.write_all(s.as_bytes())?;
        w.write_all(b"\"")
    } else {
        serde_json::to_writer(&mut *w, s).map_err(io::Error::other)
    }
}

#[inline]
fn write_u64(w: &mut impl Write, mut n: u64) -> io::Result<()> {
    let mut buf = [0u8; 20];
    let mut i = buf.len();
    loop {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    w.write_all(&buf[i..])
}

#[inline]
fn write_i64(w: &mut impl Write, n: i64) -> io::Result<()> {
    if n < 0 {
        w.write_all(b"-")?;
        write_u64(w, n.unsigned_abs())
    } else {
        write_u64(w, n as u64)
    }
}

#[inline]
fn write_kv_i64(w: &mut impl Write, key: &[u8], val: i64) -> io::Result<()> {
    w.write_all(key)?;
    write_i64(w, val)
}

pub fn export(root: &Item, attrs: &Attrs, out: &mut impl Write) -> io::Result<()> {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    writeln!(
        out,
        "[1,2,{{\"progname\":\"rsdu\",\"progver\":\"{}\",\"timestamp\":{ts}}},",
        env!("CARGO_PKG_VERSION")
    )?;
    let flags = attrs.flags();
    write_dir(root, 0, flags, out)?;
    out.write_all(b"]\n")
}

/// Directory entries follow ncdu's format: `dsize` is the directory's own usage (readers add up
/// the children), and `dev` appears on the root and wherever it differs from the parent.
fn write_dir(d: &Item, parent_dev: u32, a: AttrFlags, w: &mut impl Write) -> io::Result<()> {
    w.write_all(b"[{\"name\":")?;
    write_json_str(w, &d.name)?;
    let own = d.dir_ext().map_or(0, |e| e.own_usage);
    if a.dsize && own > 0 {
        write_kv_i64(w, b",\"dsize\":", own)?;
    }
    let dev = d.dir_ext().map_or(0, |e| e.dev);
    if dev != 0 && dev != parent_dev {
        write_kv_i64(w, b",\"dev\":", i64::from(dev))?;
    }
    if a.mtime && d.mtime != 0 {
        write_kv_i64(w, b",\"mtime\":", d.mtime)?;
    }
    if d.flag() == '!' {
        w.write_all(b",\"read_error\":true")?;
    }
    // children inherit this directory's device unless they state their own
    let eff_dev = if dev != 0 { dev } else { parent_dev };
    let children = d.children();
    if !children.is_empty() {
        w.write_all(b"},\n")?;
    } else {
        w.write_all(b"}\n")?;
    }
    for (i, c) in children.iter().enumerate() {
        if i > 0 {
            w.write_all(b",\n")?;
        }
        if c.is_dir() {
            write_dir(c, eff_dev, a, w)?;
        } else {
            write_file(c, a, w)?;
        }
    }
    w.write_all(b"]")
}

fn write_file(f: &Item, a: AttrFlags, w: &mut impl Write) -> io::Result<()> {
    w.write_all(b"{\"name\":")?;
    write_json_str(w, &f.name)?;
    if a.asize && f.size > 0 {
        write_kv_i64(w, b",\"asize\":", f.size)?;
    }
    if a.dsize && f.usage > 0 {
        write_kv_i64(w, b",\"dsize\":", f.usage)?;
    }
    if a.mtime && f.mtime != 0 {
        write_kv_i64(w, b",\"mtime\":", f.mtime)?;
    }
    let flag = f.flag();
    if a.notreg && flag == '@' {
        w.write_all(b",\"notreg\":true")?;
    }
    if a.ino && flag == 'H' {
        w.write_all(b",\"ino\":")?;
        write_u64(w, f.ino())?;
        w.write_all(b",\"hlnkc\":true")?;
    }
    w.write_all(b"}")
}

/// Single-pass streaming JSON parser (modeled after ncdu's json_import.zig) that builds `Item`s
/// directly without allocating a `serde_json::Value` DOM.
type MetaResult = Result<(Item, Option<i64>, Option<i64>, Option<i64>), String>;

struct Parser<'a> {
    b: &'a [u8],
    pos: usize,
    /// Directory sizes are cumulative (gdu, rsdu 0.1.0) instead of own-only (ncdu).
    legacy: bool,
    /// `dev` of the directory object parse_meta just read, if it had one.
    meta_dev: Option<u32>,
}

/// A name that could point outside the scanned directory when joined onto a path.
fn bad_name(n: &str) -> bool {
    n.is_empty() || n == "." || n == ".." || n.contains(['/', '\0'])
}

impl<'a> Parser<'a> {
    #[inline]
    fn ws(&mut self) {
        while self.pos < self.b.len() {
            match self.b[self.pos] {
                b' ' | b'\n' | b'\r' | b'\t' => self.pos += 1,
                _ => break,
            }
        }
    }

    #[inline]
    fn peek(&mut self) -> Option<u8> {
        self.ws();
        self.b.get(self.pos).copied()
    }

    #[inline]
    fn next_non_ws(&mut self) -> Result<u8, String> {
        self.ws();
        if self.pos < self.b.len() {
            let c = self.b[self.pos];
            self.pos += 1;
            Ok(c)
        } else {
            Err("unexpected EOF in JSON".into())
        }
    }

    #[inline]
    fn expect(&mut self, expected: u8, err: &str) -> Result<(), String> {
        let c = self.next_non_ws()?;
        if c == expected {
            Ok(())
        } else {
            Err(err.into())
        }
    }

    /// Parse a JSON string; returns a zero-copy `&'a str` when no escapes are present.
    #[inline]
    fn parse_str(&mut self) -> Result<SmallStr, String> {
        if self.next_non_ws()? != b'"' {
            return Err("expected string".into());
        }
        let start = self.pos;
        while self.pos < self.b.len() {
            let c = self.b[self.pos];
            if c == b'"' {
                let slice = &self.b[start..self.pos];
                self.pos += 1;
                return Ok(SmallStr::from_utf8_lossy(slice));
            }
            if c == b'\\' {
                return self.parse_str_escaped(start);
            }
            self.pos += 1;
        }
        Err("unterminated string".into())
    }

    #[cold]
    fn parse_str_escaped(&mut self, start: usize) -> Result<SmallStr, String> {
        // Delegate escaped strings to serde_json on the slice starting at the opening quote
        let raw = &self.b[start - 1..];
        let mut de = serde_json::Deserializer::from_slice(raw).into_iter::<String>();
        match de.next() {
            Some(Ok(s)) => {
                self.pos = (start - 1) + de.byte_offset();
                Ok(SmallStr::from(s))
            }
            _ => Err("invalid escaped string".into()),
        }
    }

    /// Parse a JSON key inside an object (opening `"` already consumed).
    #[inline]
    fn parse_key_after_quote(&mut self) -> Result<&'a [u8], String> {
        let start = self.pos;
        while self.pos < self.b.len() {
            let c = self.b[self.pos];
            if c == b'"' {
                let k = &self.b[start..self.pos];
                self.pos += 1;
                self.expect(b':', "expected ':' after key")?;
                return Ok(k);
            }
            self.pos += 1;
        }
        Err("unterminated key".into())
    }

    #[inline]
    fn parse_i64(&mut self) -> Result<i64, String> {
        self.ws();
        let start = self.pos;
        let neg = if self.pos < self.b.len() && self.b[self.pos] == b'-' {
            self.pos += 1;
            true
        } else {
            false
        };
        let mut val: i64 = 0;
        let mut digits = 0;
        while self.pos < self.b.len() {
            let c = self.b[self.pos];
            if c.is_ascii_digit() {
                val = val.wrapping_mul(10).wrapping_add(i64::from(c - b'0'));
                self.pos += 1;
                digits += 1;
            } else {
                break;
            }
        }
        if digits == 0 {
            return Err("expected number".into());
        }
        // Handle fractional/scientific notation if present
        if self.pos < self.b.len() && matches!(self.b[self.pos], b'.' | b'e' | b'E') {
            while self.pos < self.b.len()
                && matches!(
                    self.b[self.pos],
                    b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-'
                )
            {
                self.pos += 1;
            }
            let s = std::str::from_utf8(&self.b[start..self.pos]).map_err(|e| e.to_string())?;
            let f: f64 = s.parse().map_err(|_| "invalid number")?;
            return Ok(f as i64);
        }
        Ok(if neg { -val } else { val })
    }

    #[inline]
    fn parse_bool(&mut self) -> Result<bool, String> {
        self.ws();
        if self.b[self.pos..].starts_with(b"true") {
            self.pos += 4;
            Ok(true)
        } else if self.b[self.pos..].starts_with(b"false") {
            self.pos += 5;
            Ok(false)
        } else {
            Err("expected boolean".into())
        }
    }

    fn skip_value(&mut self) -> Result<(), String> {
        match self.next_non_ws()? {
            b'"' => {
                while self.pos < self.b.len() {
                    match self.b[self.pos] {
                        b'"' => {
                            self.pos += 1;
                            return Ok(());
                        }
                        b'\\' => self.pos += 2,
                        _ => self.pos += 1,
                    }
                }
                Err("unterminated string".into())
            }
            b'{' => {
                let mut depth = 1usize;
                while self.pos < self.b.len() && depth > 0 {
                    match self.b[self.pos] {
                        b'"' => {
                            self.pos += 1;
                            while self.pos < self.b.len() {
                                match self.b[self.pos] {
                                    b'"' => break,
                                    b'\\' => self.pos += 2,
                                    _ => self.pos += 1,
                                }
                            }
                        }
                        b'{' => depth += 1,
                        b'}' => depth -= 1,
                        _ => {}
                    }
                    self.pos += 1;
                }
                Ok(())
            }
            b'[' => {
                let mut depth = 1usize;
                while self.pos < self.b.len() && depth > 0 {
                    match self.b[self.pos] {
                        b'"' => {
                            self.pos += 1;
                            while self.pos < self.b.len() {
                                match self.b[self.pos] {
                                    b'"' => break,
                                    b'\\' => self.pos += 2,
                                    _ => self.pos += 1,
                                }
                            }
                        }
                        b'[' => depth += 1,
                        b']' => depth -= 1,
                        _ => {}
                    }
                    self.pos += 1;
                }
                Ok(())
            }
            _ => {
                while self.pos < self.b.len()
                    && !matches!(
                        self.b[self.pos],
                        b',' | b'}' | b']' | b' ' | b'\n' | b'\r' | b'\t'
                    )
                {
                    self.pos += 1;
                }
                Ok(())
            }
        }
    }

    fn parse_meta(&mut self, is_dir: bool) -> MetaResult {
        let mut name: Option<SmallStr> = None;
        let mut asize: Option<i64> = None;
        let mut dsize: Option<i64> = None;
        let mut items: Option<i64> = None;
        let mut mtime: i64 = 0;
        let mut ino: u64 = 0;
        let mut dev: Option<u32> = None;
        let mut read_error = false;
        let mut notreg = false;
        let mut hlnkc = false;

        let mut first = true;
        loop {
            match self.next_non_ws()? {
                b'}' => break,
                b'"' if first => {}
                b',' if !first => {
                    if self.next_non_ws()? != b'"' {
                        return Err("expected key".into());
                    }
                }
                _ => return Err("invalid object syntax".into()),
            }
            first = false;
            let k = self.parse_key_after_quote()?;
            match k {
                b"name" => {
                    if self.peek() != Some(b'"') {
                        return Err(if is_dir {
                            "directory name is not a string".into()
                        } else {
                            "file name is not a string".into()
                        });
                    }
                    name = Some(self.parse_str()?);
                }
                b"asize" => asize = self.parse_i64().ok(),
                b"dsize" => dsize = self.parse_i64().ok(),
                b"items" => items = self.parse_i64().ok(),
                b"mtime" => mtime = self.parse_i64().unwrap_or(0),
                b"ino" => ino = self.parse_i64().unwrap_or(0) as u64,
                b"dev" => dev = self.parse_i64().ok().map(|v| fold_dev(v as u64)),
                b"read_error" => read_error = self.parse_bool().unwrap_or(false),
                b"notreg" => {
                    let _ = self.skip_value();
                    notreg = true;
                }
                b"hlnkc" => {
                    let _ = self.skip_value();
                    hlnkc = true;
                }
                _ => self.skip_value()?,
            }
        }

        let Some(name) = name else {
            return Err(if is_dir {
                "directory name is not a string".into()
            } else {
                "file name is not a string".into()
            });
        };

        if is_dir {
            self.meta_dev = dev;
            let mut d = Item::new_dir(name, if read_error { '!' } else { ' ' });
            d.mtime = mtime;
            Ok((d, asize, dsize, items))
        } else {
            if bad_name(&name) {
                return Err("invalid item name".into());
            }
            let flag = if hlnkc {
                'H'
            } else if notreg {
                '@'
            } else {
                ' '
            };
            let f = Item::new_file(
                name,
                flag,
                asize.unwrap_or(0),
                dsize.unwrap_or(0),
                mtime,
                if hlnkc { ino } else { 0 },
            );
            Ok((f, asize, dsize, items))
        }
    }

    fn parse_dir(&mut self, inherited_dev: u32) -> Result<Item, String> {
        match self.next_non_ws()? {
            b']' => return Err("directory array is empty".into()),
            b'{' => {}
            _ => return Err("directory item is not a map".into()),
        }
        let (mut d, size, usage, count) = self.parse_meta(true)?;
        let dev = self.meta_dev.take().unwrap_or(inherited_dev);
        if let Some(ext) = d.dir_ext_mut() {
            ext.dev = dev;
        }

        loop {
            match self.next_non_ws()? {
                b']' => break,
                b',' => match self.next_non_ws()? {
                    b'{' => {
                        let (f, _, _, _) = self.parse_meta(false)?;
                        d.children_mut().push(f);
                    }
                    b'[' => {
                        let mut c = self.parse_dir(dev)?;
                        if let Some(p) = c.name.rfind('/') {
                            c.name = SmallStr::from(&c.name[p + 1..]);
                        }
                        if bad_name(&c.name) {
                            return Err("invalid item name".into());
                        }
                        d.children_mut().push(c);
                    }
                    _ => self.skip_value()?,
                },
                _ => return Err("expected ',' or ']' in directory array".into()),
            }
        }

        if let Some(u) = usage {
            if self.legacy {
                // cumulative total: what is left after the children is the directory's own usage
                d.usage = u;
                let kids: i64 = d.children().iter().map(|c| c.usage).sum();
                if let Some(ext) = d.dir_ext_mut() {
                    ext.own_usage = (u - kids).max(0);
                }
            } else if let Some(ext) = d.dir_ext_mut() {
                ext.own_usage = u.max(0);
            }
        }
        // depth-truncated leaf (cumulative formats only): pin the exported totals
        if let (true, Some(sz), Some(u), Some(n), true) =
            (self.legacy, size, usage, count, d.children().is_empty())
            && !(n == 1 && sz == EMPTY_DIR_SIZE)
        {
            d.size = sz;
            d.usage = u;
            if let Some(ext) = d.dir_ext_mut() {
                ext.items = n;
                ext.fixed_stats = true;
            }
        }
        Ok(d)
    }
}

pub fn import(input: &str) -> Result<Item, String> {
    let mut p = Parser {
        b: input.as_bytes(),
        pos: 0,
        legacy: false,
        meta_dev: None,
    };
    if p.next_non_ws()? != b'[' {
        return Err("JSON file does not contain top level array".into());
    }
    // Elements 0, 1, 2 (major, minor, metadata)
    for i in 0..3 {
        if p.peek() == Some(b']') {
            return Err("top level array must have at least 4 items".into());
        }
        p.ws();
        let start = p.pos;
        p.skip_value()?;
        if i == 2 {
            // gdu and rsdu 0.1.0 wrote cumulative directory totals; ncdu and newer rsdu own sizes
            if let Ok(meta) = serde_json::from_slice::<serde_json::Value>(&p.b[start..p.pos]) {
                let name = meta["progname"].as_str().unwrap_or("");
                let ver = meta["progver"].as_str().unwrap_or("");
                p.legacy = name == "gdu" || (name == "rsdu" && ver == "0.1.0");
            }
        }
        if p.next_non_ws()? != b',' {
            return Err("top level array must have at least 4 items".into());
        }
    }
    if p.next_non_ws()? != b'[' {
        return Err("array of maps not found in the top level array on 4th position".into());
    }
    let mut root = p.parse_dir(0)?;
    root.update_stats();
    Ok(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> Item {
        let mk = |n: &str, sz| Item::new_file(n, ' ', sz, sz, 5, 0);
        let hl = |n: &str| Item::new_file(n, 'H', 40, 4096, 5, 77);
        let mut root = Item::new_dir("/r", ' ');
        root.dir_ext_mut().unwrap().own_usage = 4096;
        let mut sub = Item::new_dir("s", ' ');
        sub.dir_ext_mut().unwrap().own_usage = 4096;
        sub.children_mut().push(mk("f", 100));
        sub.children_mut().push(hl("link-a"));
        root.children_mut().push(sub);
        root.children_mut().push(mk("g", 7));
        root.children_mut().push(hl("link-b"));
        root.update_stats();
        root
    }

    fn totals(i: &Item) -> (i64, i64, i64) {
        (i.size, i.usage, i.items())
    }

    #[test]
    fn roundtrip() {
        let root = tree();
        let mut buf = Vec::new();
        export(&root, &Attrs::default(), &mut buf).unwrap();
        let text = std::str::from_utf8(&buf).unwrap();
        // ncdu semantics: directories carry only their own usage, never a cumulative total
        assert!(text.contains(r#"{"name":"/r","dsize":4096"#), "{text}");
        assert!(!text.contains("\"items\""), "{text}");
        let back = import(text).unwrap();
        assert_eq!(totals(&back), totals(&root));
        assert_eq!(back.children()[0].children()[0].name, "f");
        // the hardlinked inode is counted once for the whole tree, once for the subdir
        assert_eq!(root.usage, 4096 + 4096 + 100 + 7 + 4096);
    }

    #[test]
    fn imports_ncdu_own_sizes() {
        let j = r#"[1,2,{"progname":"ncdu","progver":"2.9.1","timestamp":1},
[{"name":"/r","dsize":4096},
{"name":"a","asize":10,"dsize":4096},
[{"name":"d","asize":4096,"dsize":4096,"dev":9},
{"name":"f","asize":100,"dsize":8192}]]]"#;
        let r = import(j).unwrap();
        assert_eq!(r.usage, 4096 + 4096 + 4096 + 8192);
        assert_eq!(r.size, 10 + 100);
        assert_eq!(r.items(), 4);
        // `dev` is inherited by everything below the directory that states it
        assert_eq!(r.children()[1].dir_ext().unwrap().dev, fold_dev(9));
    }

    #[test]
    fn imports_legacy_cumulative_totals() {
        // gdu writes the cumulative size for directories; own usage is what the children leave
        let j = r#"[1,2,{"progname":"gdu","progver":"5.32.0","timestamp":1},
[{"name":"/r","asize":10,"dsize":8192,"items":2},
{"name":"a","asize":10,"dsize":4096}]]"#;
        let r = import(j).unwrap();
        assert_eq!((r.size, r.usage), (10, 8192));
    }

    #[test]
    fn rejects_names_that_escape_the_root() {
        for bad in ["../x", "/etc/passwd", "a/b", "..", "."] {
            let j = format!(
                r#"[1,2,{{"progname":"ncdu"}},[{{"name":"/r"}},{{"name":{bad:?},"asize":1}}]]"#
            );
            assert_eq!(import(&j).unwrap_err(), "invalid item name", "{bad}");
        }
        let j = r#"[1,2,{"progname":"ncdu"},[{"name":"/r"},[{"name":".."}]]]"#;
        assert_eq!(import(j).unwrap_err(), "invalid item name");
    }
}
