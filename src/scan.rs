// stat field types differ per platform (Linux vs macOS), so the casts are needed
#![allow(clippy::unnecessary_cast)]

use crate::ignore::Ignore;
use crate::tree::{EMPTY_DIR_SIZE, Item, LinkMap, SmallStr, merge_links};
use rayon::prelude::*;
use rustix::fd::{AsFd, BorrowedFd, OwnedFd};
#[cfg(not(target_os = "linux"))]
use rustix::fs::Dir;
#[cfg(target_os = "linux")]
use rustix::fs::RawDir;
use rustix::fs::{AtFlags, CWD, FileType, Mode, OFlags, Stat, fstat, openat, statat};
use std::ffi::{CStr, OsStr};
use std::fs;
#[cfg(target_os = "linux")]
use std::mem::MaybeUninit;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

#[derive(Default)]
pub struct Progress {
    pub items: AtomicU64,
    pub bytes: AtomicU64,
    pub active: bool,
    pub current: Mutex<String>,
}

impl Progress {
    pub fn active() -> Self {
        Self {
            active: true,
            ..Default::default()
        }
    }
}

#[derive(Default)]
pub struct Opts {
    pub ignore: Ignore,
    pub no_cross: bool,
    /// Maximum tree depth to retain child `Item`s for (`None` keeps full tree).
    pub max_depth: Option<usize>,
}

/// File item from raw stat fields; `special` is a symlink or socket.
#[allow(clippy::too_many_arguments)]
#[inline]
pub fn make_file(
    name: impl Into<SmallStr>,
    special: bool,
    size: i64,
    blocks: i64,
    mtime: i64,
    nlink: u64,
    ino: u64,
) -> Item {
    let flag = if special {
        '@'
    } else if nlink > 1 {
        'H'
    } else {
        ' '
    };
    Item::new_file(
        name,
        flag,
        size,
        blocks * 512,
        mtime,
        if nlink > 1 { ino } else { 0 },
    )
}

/// File item from std metadata (lstat).
pub fn file_item(name: String, md: &fs::Metadata) -> Item {
    let ft = md.file_type();
    make_file(
        SmallStr::from(name),
        ft.is_symlink() || ft.is_socket(),
        md.size() as i64,
        md.blocks() as i64,
        md.mtime(),
        md.nlink(),
        md.ino(),
    )
}

#[inline]
fn file_item_st(name: SmallStr, st: &Stat) -> Item {
    let ft = FileType::from_raw_mode(st.st_mode as _);
    make_file(
        name,
        matches!(ft, FileType::Symlink | FileType::Socket),
        st.st_size as i64,
        st.st_blocks as i64,
        st.st_mtime as i64,
        st.st_nlink as u64,
        st.st_ino as u64,
    )
}

/// Raise soft file descriptor limit so deep parallel parent-FD traversal never hits EMFILE.
fn raise_fd_limit() {
    unsafe {
        let mut rlim: libc::rlimit = std::mem::zeroed();
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut rlim) == 0 && rlim.rlim_cur < rlim.rlim_max {
            // On macOS, OPEN_MAX (10240) is the cap for unprivileged setrlimit
            #[cfg(target_os = "macos")]
            let target = rlim.rlim_max.min(10240);
            #[cfg(not(target_os = "macos"))]
            let target = rlim.rlim_max;
            rlim.rlim_cur = target;
            let _ = libc::setrlimit(libc::RLIMIT_NOFILE, &rlim);
        }
    }
}

/// Scan `path` (should be absolute); the root item is named by its full path.
pub fn scan(path: &Path, opts: &Opts, prog: &Progress) -> Item {
    raise_fd_limit();
    let root_str = path.to_string_lossy();
    let scoped_opts = Opts {
        ignore: opts.ignore.scoped_to(&root_str),
        no_cross: opts.no_cross,
        max_depth: opts.max_depth,
    };
    let need_path = scoped_opts.ignore.has_path_rules() || prog.active;
    let keep_depth = scoped_opts.max_depth.unwrap_or(usize::MAX);
    match scan_dir(
        None,
        Some(path.as_os_str()),
        Some(path),
        SmallStr::from(root_str),
        0,
        keep_depth,
        need_path,
        None,
        None,
        &scoped_opts,
        prog,
    ) {
        Some(ScanOut::Node(root, _)) => root,
        _ => Item::new_dir(SmallStr::default(), '!'),
    }
}

enum ScanOut {
    Node(Item, LinkMap<(u64, u64)>),
    Stats {
        size: i64,
        usage: i64,
        items: i64,
        mtime: i64,
        flag: char,
        links: LinkMap<(u64, u64)>,
    },
}

struct SubdirEntry {
    name: SmallStr,
    raw_os: Option<Box<OsStr>>,
    path: Option<PathBuf>,
    pre_stat: Option<(u64, i64, i64)>,
}

struct DirAcc {
    total_size: i64,
    total_usage: i64,
    total_items: i64,
    max_mtime: i64,
    scanned_bytes: u64,
    entry_count: u64,
    had_entries: bool,
    flag: char,
    links: LinkMap<(u64, u64)>,
}

#[allow(clippy::too_many_arguments)]
#[inline]
fn process_entry(
    fd: &OwnedFd,
    cname: &CStr,
    mut ft: FileType,
    parent_path: Option<&Path>,
    keep_children: bool,
    need_path: bool,
    opts: &Opts,
    children: &mut Vec<Item>,
    subdirs: &mut Vec<SubdirEntry>,
    acc: &mut DirAcc,
) {
    let bytes = cname.to_bytes();
    if bytes.first() == Some(&b'.') && (bytes.len() == 1 || bytes == b".." || opts.ignore.no_hidden)
    {
        return;
    }
    let mut st = None;
    if ft == FileType::Unknown {
        // filesystem gave no d_type: ask with a stat
        match statat(fd, cname, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(s) => {
                ft = FileType::from_raw_mode(s.st_mode as _);
                st = Some(s);
            }
            Err(_) => {
                acc.flag = '!';
                return;
            }
        }
    }
    if ft == FileType::Directory {
        let p = if need_path {
            let joined = parent_path.unwrap().join(OsStr::from_bytes(bytes));
            if opts.ignore.has_path_rules() && opts.ignore.skip_path(&joined.to_string_lossy()) {
                return;
            }
            Some(joined)
        } else {
            None
        };
        let (fname, raw_os) = match std::str::from_utf8(bytes) {
            Ok(s) => (SmallStr::from(s), None),
            Err(_) => (
                SmallStr::from(String::from_utf8_lossy(bytes).into_owned()),
                Some(Box::from(OsStr::from_bytes(bytes))),
            ),
        };
        let pre_stat = st.map(|s| (s.st_dev as u64, s.st_mtime as i64, s.st_blocks as i64));
        subdirs.push(SubdirEntry {
            name: fname,
            raw_os,
            path: p,
            pre_stat,
        });
        acc.entry_count += 1;
        return;
    }
    let st = match st {
        Some(s) => s,
        None => match statat(fd, cname, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(s) => s,
            Err(_) => {
                acc.flag = '!';
                return;
            }
        },
    };
    acc.had_entries = true;
    acc.entry_count += 1;
    let fsize = st.st_size as i64;
    let fusage = st.st_blocks as i64 * 512;
    let fmtime = st.st_mtime as i64;
    let nlink = st.st_nlink as u64;
    acc.total_size += fsize;
    acc.total_usage += fusage;
    // a second name for an inode already seen in this directory adds nothing
    if nlink > 1
        && acc
            .links
            .insert((st.st_dev as u64, st.st_ino as u64), (fsize, fusage))
            .is_some()
    {
        acc.total_size -= fsize;
        acc.total_usage -= fusage;
    }
    acc.total_items += 1;
    if fmtime > acc.max_mtime {
        acc.max_mtime = fmtime;
    }
    acc.scanned_bytes += fusage as u64;
    if keep_children {
        let fname = SmallStr::from_utf8_lossy(bytes);
        children.push(file_item_st(fname, &st));
    }
}

/// Separate stack frame so the 32 KiB `getdents64` buffer is popped before recursion.
#[cfg(target_os = "linux")]
#[inline(never)]
#[allow(clippy::too_many_arguments)]
fn read_entries(
    fd: &OwnedFd,
    parent_path: Option<&Path>,
    keep_children: bool,
    need_path: bool,
    opts: &Opts,
    children: &mut Vec<Item>,
    subdirs: &mut Vec<SubdirEntry>,
    acc: &mut DirAcc,
) {
    let mut buf = [MaybeUninit::<u8>::uninit(); 32768];
    let mut rd = RawDir::new(fd, &mut buf);
    while let Some(entry) = rd.next() {
        let Ok(entry) = entry else {
            acc.flag = '!';
            continue;
        };
        process_entry(
            fd,
            entry.file_name(),
            entry.file_type(),
            parent_path,
            keep_children,
            need_path,
            opts,
            children,
            subdirs,
            acc,
        );
    }
}

#[cfg(not(target_os = "linux"))]
#[inline(never)]
#[allow(clippy::too_many_arguments)]
fn read_entries(
    fd: &OwnedFd,
    parent_path: Option<&Path>,
    keep_children: bool,
    need_path: bool,
    opts: &Opts,
    children: &mut Vec<Item>,
    subdirs: &mut Vec<SubdirEntry>,
    acc: &mut DirAcc,
) {
    let Ok(dup_fd) = rustix::io::dup(fd) else {
        acc.flag = '!';
        return;
    };
    let Ok(rd) = Dir::new(dup_fd) else {
        acc.flag = '!';
        return;
    };
    for entry in rd {
        let Ok(entry) = entry else {
            acc.flag = '!';
            continue;
        };
        process_entry(
            fd,
            entry.file_name(),
            entry.file_type(),
            parent_path,
            keep_children,
            need_path,
            opts,
            children,
            subdirs,
            acc,
        );
    }
}

/// `root_dev` is None for the scan root, which defines the device for `--no-cross`.
#[allow(clippy::too_many_arguments)]
fn scan_dir(
    parent_fd: Option<BorrowedFd<'_>>,
    raw_os: Option<&OsStr>,
    path: Option<&Path>,
    name: SmallStr,
    depth: usize,
    keep_depth: usize,
    need_path: bool,
    pre_stat: Option<(u64, i64, i64)>,
    root_dev: Option<u64>,
    opts: &Opts,
    prog: &Progress,
) -> Option<ScanOut> {
    let oflags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
    let open_name: &OsStr = raw_os.unwrap_or_else(|| OsStr::new(name.as_str()));
    let fd = match parent_fd {
        Some(pfd) => openat(pfd, open_name, oflags | OFlags::NOFOLLOW, Mode::empty()).ok(),
        None => openat(CWD, open_name, oflags, Mode::empty()).ok(),
    };

    // dir's own stat: reuse pre_stat if already queried, else from fd, else via parent_fd/path
    let (dev, mtime, blocks) = match pre_stat {
        Some((d, m, b)) => (Some(d), m, b),
        None => match fd.as_ref().and_then(|fd| fstat(fd).ok()) {
            Some(st) => (
                Some(st.st_dev as u64),
                st.st_mtime as i64,
                st.st_blocks as i64,
            ),
            None => match parent_fd {
                Some(pfd) => match statat(pfd, open_name, AtFlags::SYMLINK_NOFOLLOW) {
                    Ok(st) => (
                        Some(st.st_dev as u64),
                        st.st_mtime as i64,
                        st.st_blocks as i64,
                    ),
                    Err(_) => (None, 0, 0),
                },
                None => match path.and_then(|p| fs::symlink_metadata(p).ok()) {
                    Some(m) => (Some(m.dev()), m.mtime(), m.blocks() as i64),
                    None => (None, 0, 0),
                },
            },
        },
    };
    let my_dev = root_dev.or(dev);
    if opts.no_cross && root_dev.is_some() && dev.is_some_and(|d| Some(d) != root_dev) {
        return None;
    }
    let own_usage = blocks * 512;
    let keep_node = depth <= keep_depth;

    let Some(fd) = fd else {
        if keep_node {
            let mut dir = Item::new_dir(name, '!');
            dir.mtime = mtime;
            dir.size = EMPTY_DIR_SIZE;
            dir.usage = own_usage;
            if let Some(ext) = dir.dir_ext_mut() {
                ext.own_usage = own_usage;
            }
            return Some(ScanOut::Node(dir, LinkMap::default()));
        }
        return Some(ScanOut::Stats {
            size: EMPTY_DIR_SIZE,
            usage: own_usage,
            items: 1,
            mtime,
            flag: '!',
            links: LinkMap::default(),
        });
    };

    let keep_children = depth < keep_depth;
    let mut children = Vec::new();
    let mut subdirs = Vec::new();
    let mut acc = DirAcc {
        total_size: 0,
        total_usage: own_usage,
        total_items: 1,
        max_mtime: mtime,
        scanned_bytes: 0,
        entry_count: 0,
        had_entries: false,
        flag: ' ',
        links: LinkMap::default(),
    };

    read_entries(
        &fd,
        path,
        keep_children,
        need_path,
        opts,
        &mut children,
        &mut subdirs,
        &mut acc,
    );

    if prog.active && acc.entry_count > 0 {
        prog.bytes.fetch_add(acc.scanned_bytes, Relaxed);
        let prev = prog.items.fetch_add(acc.entry_count, Relaxed);
        if (prev ^ (prev + acc.entry_count)) >= 512
            && let Some(p) = path
            && let Ok(mut cur) = prog.current.try_lock()
        {
            cur.clear();
            cur.push_str(&p.to_string_lossy());
        }
    }

    let mut has_err_child = false;
    let pfd = fd.as_fd();
    match subdirs.len() {
        0 => {}
        1 => {
            let s = subdirs.pop().unwrap();
            if let Some(sub) = scan_dir(
                Some(pfd),
                s.raw_os.as_deref(),
                s.path.as_deref(),
                s.name,
                depth + 1,
                keep_depth,
                need_path,
                s.pre_stat,
                my_dev,
                opts,
                prog,
            ) {
                acc.had_entries = true;
                match sub {
                    ScanOut::Node(item, links) => {
                        merge_links(
                            &mut acc.total_size,
                            &mut acc.total_usage,
                            &mut acc.links,
                            links,
                        );
                        acc.total_size += item.size;
                        acc.total_usage += item.usage;
                        acc.total_items += item.items();
                        if item.mtime > acc.max_mtime {
                            acc.max_mtime = item.mtime;
                        }
                        let sf = item.flag();
                        if sf == '!' || sf == '.' {
                            has_err_child = true;
                        }
                        if keep_children {
                            children.push(item);
                        }
                    }
                    ScanOut::Stats {
                        size,
                        usage,
                        items,
                        mtime: smtime,
                        flag: sf,
                        links,
                    } => {
                        merge_links(
                            &mut acc.total_size,
                            &mut acc.total_usage,
                            &mut acc.links,
                            links,
                        );
                        acc.total_size += size;
                        acc.total_usage += usage;
                        acc.total_items += items;
                        if smtime > acc.max_mtime {
                            acc.max_mtime = smtime;
                        }
                        if sf == '!' || sf == '.' {
                            has_err_child = true;
                        }
                    }
                }
            }
        }
        _ => {
            let subs: Vec<ScanOut> = subdirs
                .into_par_iter()
                .filter_map(|s| {
                    scan_dir(
                        Some(pfd),
                        s.raw_os.as_deref(),
                        s.path.as_deref(),
                        s.name,
                        depth + 1,
                        keep_depth,
                        need_path,
                        s.pre_stat,
                        my_dev,
                        opts,
                        prog,
                    )
                })
                .collect();
            if keep_children {
                children.reserve(subs.len());
            }
            for sub in subs {
                acc.had_entries = true;
                match sub {
                    ScanOut::Node(item, links) => {
                        merge_links(
                            &mut acc.total_size,
                            &mut acc.total_usage,
                            &mut acc.links,
                            links,
                        );
                        acc.total_size += item.size;
                        acc.total_usage += item.usage;
                        acc.total_items += item.items();
                        if item.mtime > acc.max_mtime {
                            acc.max_mtime = item.mtime;
                        }
                        let sf = item.flag();
                        if sf == '!' || sf == '.' {
                            has_err_child = true;
                        }
                        if keep_children {
                            children.push(item);
                        }
                    }
                    ScanOut::Stats {
                        size,
                        usage,
                        items,
                        mtime: smtime,
                        flag: sf,
                        links,
                    } => {
                        merge_links(
                            &mut acc.total_size,
                            &mut acc.total_usage,
                            &mut acc.links,
                            links,
                        );
                        acc.total_size += size;
                        acc.total_usage += usage;
                        acc.total_items += items;
                        if smtime > acc.max_mtime {
                            acc.max_mtime = smtime;
                        }
                        if sf == '!' || sf == '.' {
                            has_err_child = true;
                        }
                    }
                }
            }
        }
    }
    drop(fd);

    if !acc.had_entries {
        acc.total_size += EMPTY_DIR_SIZE;
        if acc.flag == ' ' {
            acc.flag = 'e';
        }
    } else if has_err_child && acc.flag != '!' {
        acc.flag = '.';
    }

    if keep_node {
        let mut dir = Item::new_dir(name, acc.flag);
        dir.size = acc.total_size;
        dir.usage = acc.total_usage;
        dir.mtime = acc.max_mtime;
        if let Some(ext) = dir.dir_ext_mut() {
            ext.own_usage = own_usage;
            ext.items = acc.total_items;
            ext.children = children;
        }
        Some(ScanOut::Node(dir, acc.links))
    } else {
        Some(ScanOut::Stats {
            size: acc.total_size,
            usage: acc.total_usage,
            items: acc.total_items,
            mtime: acc.max_mtime,
            flag: acc.flag,
            links: acc.links,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_tree() {
        let d = std::env::temp_dir().join(format!("rsdu-scan-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("a/b")).unwrap();
        fs::create_dir(d.join("empty")).unwrap();
        fs::write(d.join("a/f"), "hello").unwrap();
        fs::hard_link(d.join("a/f"), d.join("hl")).unwrap();
        fs::write(d.join("a/b/big"), vec![0u8; 10000]).unwrap();
        std::os::unix::fs::symlink("a/f", d.join("ln")).unwrap();

        let o = Opts::default();
        let r = scan(&d, &o, &Progress::default());
        let ch = |n: &str| r.children().iter().find(|c| c.name == n).unwrap();
        assert_eq!(ch("empty").flag(), 'e');
        assert_eq!(ch("ln").flag(), '@');
        assert_eq!(ch("hl").flag(), 'H');
        // root + a + b + big + f + hl + ln + empty
        assert_eq!(r.items(), 8);
        // hardlink pair counted once: 5 + 10000 + symlink len(3) + empty dir(512)
        assert_eq!(r.size, 5 + 10000 + 3 + 512);

        // Verify depth-pruned scan (max_depth = Some(0) and Some(1)) produces identical root totals
        let r0 = scan(
            &d,
            &Opts {
                max_depth: Some(0),
                ..Default::default()
            },
            &Progress::default(),
        );
        assert_eq!(
            (r0.size, r0.usage, r0.items()),
            (r.size, r.usage, r.items())
        );
        let r1 = scan(
            &d,
            &Opts {
                max_depth: Some(1),
                ..Default::default()
            },
            &Progress::default(),
        );
        assert_eq!(
            (r1.size, r1.usage, r1.items()),
            (r.size, r.usage, r.items())
        );
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn unreadable_dir_is_flagged() {
        use std::os::unix::fs::PermissionsExt;
        let d = std::env::temp_dir().join(format!("rsdu-perm-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("locked")).unwrap();
        fs::write(d.join("ok"), "x").unwrap();
        fs::set_permissions(d.join("locked"), fs::Permissions::from_mode(0o000)).unwrap();
        // root can read anything, so there is nothing to check
        if fs::read_dir(d.join("locked")).is_err() {
            let o = Opts::default();
            let r = scan(&d, &o, &Progress::default());
            let locked = r.children().iter().find(|c| c.name == "locked").unwrap();
            assert_eq!(locked.flag(), '!');
            assert_eq!(r.flag(), '.');
        }
        fs::set_permissions(d.join("locked"), fs::Permissions::from_mode(0o755)).unwrap();
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn cross_dir_hardlink_counted_once_per_subtree() {
        let d = std::env::temp_dir().join(format!("rsdu-xlink-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("a")).unwrap();
        fs::create_dir_all(d.join("b")).unwrap();
        fs::write(d.join("a/f"), vec![1u8; 5000]).unwrap();
        fs::hard_link(d.join("a/f"), d.join("b/f")).unwrap();
        let o = Opts {
            ignore: Ignore::default(),
            no_cross: false,
            max_depth: None,
        };
        // every run must give the same answer whatever order the threads finish in
        for _ in 0..20 {
            let r = scan(&d, &o, &Progress::default());
            let size = |n: &str| r.children().iter().find(|c| &*c.name == n).unwrap().size;
            assert_eq!(size("a"), 5000);
            assert_eq!(size("b"), 5000);
            assert_eq!(r.size, 5000);
        }
        fs::remove_dir_all(&d).unwrap();
    }
}
