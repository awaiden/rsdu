
pub struct Device {
    pub name: String,
    pub mount: String,
    pub size: i64,
    /// Blocks in use (reserved-for-root blocks count as used, like `df`).
    pub used: i64,
    /// Blocks available to unprivileged users.
    pub free: i64,
}

/// Mounted block/zfs/nfs filesystems from /proc/self/mountinfo (snap mounts skipped).
pub fn list() -> std::io::Result<Vec<Device>> {
    if !cfg!(target_os = "linux") {
        return Err(std::io::Error::other(
            "device listing is only supported on Linux",
        ));
    }
    let text = std::fs::read_to_string("/proc/self/mountinfo")?;
    let mut out = Vec::new();
    for line in text.lines() {
        let p: Vec<&str> = line.split_whitespace().collect();
        let Some(sep) = p.iter().position(|x| *x == "-") else {
            continue;
        };
        if sep < 6 || p.len() < sep + 3 {
            continue;
        }
        let (fstype, name, mount) = (p[sep + 1], p[sep + 2], unescape(p[4]));
        if mount.contains("/snap/")
            || !(name.starts_with("/dev") || ["zfs", "nfs", "nfs4"].contains(&fstype))
        {
            continue;
        }
        let Ok(st) = rustix::fs::statvfs(mount.as_str()) else {
            continue;
        };
        let bs = st.f_frsize as i64;
        out.push(Device {
            name: name.into(),
            mount,
            size: bs * st.f_blocks as i64,
            used: bs * (st.f_blocks as i64 - st.f_bfree as i64),
            free: bs * st.f_bavail as i64,
        });
    }
    Ok(out)
}

/// mountinfo escapes space, tab, newline and backslash as `\NNN` (octal).
fn unescape(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\'
            && i + 3 < b.len()
            && b[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c))
        {
            let v = (b[i + 1] - b'0') as u32 * 64
                + (b[i + 2] - b'0') as u32 * 8
                + (b[i + 3] - b'0') as u32;
            out.push(v as u8);
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::unescape;

    #[test]
    fn decodes_octal_escapes() {
        assert_eq!(unescape("/mnt/my\\040disk"), "/mnt/my disk");
        assert_eq!(unescape("/a\\011b\\134c"), "/a\tb\\c");
        assert_eq!(unescape("/plain"), "/plain");
        assert_eq!(unescape("/trail\\04"), "/trail\\04");
    }
}
