use std::ffi::CString;

pub struct Device {
    pub name: String,
    pub mount: String,
    pub size: i64,
    pub free: i64,
}

/// Mounted block/zfs/nfs filesystems from /proc/self/mountinfo (snap mounts skipped).
pub fn list() -> std::io::Result<Vec<Device>> {
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
        let (fstype, name, mount) = (p[sep + 1], p[sep + 2], p[4].replace("\\040", " "));
        if mount.contains("/snap/")
            || !(name.starts_with("/dev") || ["zfs", "nfs", "nfs4"].contains(&fstype))
        {
            continue;
        }
        let Ok(c) = CString::new(mount.clone()) else {
            continue;
        };
        let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
            continue;
        }
        let bs = st.f_frsize as i64;
        out.push(Device {
            name: name.into(),
            mount,
            size: bs * st.f_blocks as i64,
            free: bs * st.f_bavail as i64,
        });
    }
    Ok(out)
}
