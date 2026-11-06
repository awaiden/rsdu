use std::borrow::{Borrow, Cow};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::fmt;
use std::hash::{BuildHasherDefault, Hash, Hasher};
use std::ops::Deref;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

/// Apparent size gdu gives an empty directory.
pub const EMPTY_DIR_SIZE: i64 = 512;

/// Fold a 64-bit `st_dev` into the 32 bits stored per item (hardlink identity is `(dev, ino)`).
#[inline]
pub fn fold_dev(dev: u64) -> u32 {
    (dev ^ (dev >> 32)) as u32
}

/// 24-byte string that stores names <= 22 bytes inline on the stack without heap allocation.
#[derive(Clone)]
pub enum SmallStr {
    Inline(u8, [u8; 22]),
    Heap(Box<str>),
    /// Name that is not valid UTF-8: `display` is the lossy form shown on screen,
    /// `bytes` the real name used to open or delete the file.
    Raw(Box<RawName>),
}

#[derive(Clone)]
pub struct RawName {
    display: Box<str>,
    bytes: Box<[u8]>,
}

impl SmallStr {
    #[inline]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Inline(len, buf) => {
                // SAFETY: `buf[..*len as usize]` is only ever populated from valid UTF-8 `&str`.
                unsafe { std::str::from_utf8_unchecked(&buf[..*len as usize]) }
            }
            Self::Heap(s) => s,
            Self::Raw(r) => &r.display,
        }
    }

    /// The real on-disk name (differs from `as_str` only for `Raw`).
    #[inline]
    pub fn as_os_str(&self) -> &OsStr {
        match self {
            Self::Raw(r) => OsStr::from_bytes(&r.bytes),
            _ => OsStr::new(self.as_str()),
        }
    }

    #[inline]
    pub fn from_utf8_lossy(bytes: &[u8]) -> Self {
        match std::str::from_utf8(bytes) {
            Ok(s) => Self::from(s),
            Err(_) => Self::Raw(Box::new(RawName {
                display: String::from_utf8_lossy(bytes).into(),
                bytes: bytes.into(),
            })),
        }
    }
}

impl Default for SmallStr {
    #[inline]
    fn default() -> Self {
        Self::Inline(0, [0; 22])
    }
}

impl Deref for SmallStr {
    type Target = str;
    #[inline]
    fn deref(&self) -> &str {
        self.as_str()
    }
}

impl AsRef<str> for SmallStr {
    #[inline]
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl AsRef<OsStr> for SmallStr {
    #[inline]
    fn as_ref(&self) -> &OsStr {
        self.as_os_str()
    }
}

impl AsRef<Path> for SmallStr {
    #[inline]
    fn as_ref(&self) -> &Path {
        Path::new(self.as_os_str())
    }
}

impl Borrow<str> for SmallStr {
    #[inline]
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl From<&str> for SmallStr {
    #[inline]
    fn from(s: &str) -> Self {
        let b = s.as_bytes();
        if b.len() <= 22 {
            let mut buf = [0u8; 22];
            buf[..b.len()].copy_from_slice(b);
            Self::Inline(b.len() as u8, buf)
        } else {
            Self::Heap(s.into())
        }
    }
}

impl From<String> for SmallStr {
    #[inline]
    fn from(s: String) -> Self {
        let b = s.as_bytes();
        if b.len() <= 22 {
            let mut buf = [0u8; 22];
            buf[..b.len()].copy_from_slice(b);
            Self::Inline(b.len() as u8, buf)
        } else {
            Self::Heap(s.into_boxed_str())
        }
    }
}

impl From<Box<str>> for SmallStr {
    #[inline]
    fn from(s: Box<str>) -> Self {
        let b = s.as_bytes();
        if b.len() <= 22 {
            let mut buf = [0u8; 22];
            buf[..b.len()].copy_from_slice(b);
            Self::Inline(b.len() as u8, buf)
        } else {
            Self::Heap(s)
        }
    }
}

impl From<Cow<'_, str>> for SmallStr {
    #[inline]
    fn from(c: Cow<'_, str>) -> Self {
        match c {
            Cow::Borrowed(s) => Self::from(s),
            Cow::Owned(s) => Self::from(s),
        }
    }
}

impl From<SmallStr> for String {
    #[inline]
    fn from(s: SmallStr) -> Self {
        match s {
            SmallStr::Heap(b) => b.into_string(),
            _ => s.as_str().to_owned(),
        }
    }
}

impl PartialEq for SmallStr {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.as_str() == other.as_str()
    }
}

impl Eq for SmallStr {}

impl PartialEq<str> for SmallStr {
    #[inline]
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<&str> for SmallStr {
    #[inline]
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl PartialOrd for SmallStr {
    #[inline]
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for SmallStr {
    #[inline]
    fn cmp(&self, other: &Self) -> Ordering {
        self.as_str().cmp(other.as_str())
    }
}

impl Hash for SmallStr {
    #[inline]
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.as_str().hash(state);
    }
}

impl fmt::Display for SmallStr {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl fmt::Debug for SmallStr {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), f)
    }
}

/// Fast multiply-shift hasher for `u64` inode numbers.
#[derive(Default)]
pub struct FxHasher(u64);

impl Hasher for FxHasher {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0.rotate_left(5) ^ u64::from(b)).wrapping_mul(0x517cc1b727220a95);
        }
    }

    #[inline]
    fn write_u64(&mut self, i: u64) {
        self.0 = (self.0.rotate_left(5) ^ i).wrapping_mul(0x517cc1b727220a95);
    }

    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }
}

/// Hardlinked inodes found in a subtree, with their (apparent, disk) size.
/// Each directory counts an inode once however many names it has below it, so totals
/// do not depend on the order (or threads) in which the tree was visited.
pub type LinkMap<K> = HashMap<K, (i64, i64), BuildHasherDefault<FxHasher>>;

/// Fold `from` into `into`. The caller already added the totals of both sides, so every
/// inode present on both sides is taken off once.
pub fn merge_links<K: Hash + Eq>(
    size: &mut i64,
    usage: &mut i64,
    into: &mut LinkMap<K>,
    mut from: LinkMap<K>,
) {
    if into.len() < from.len() {
        std::mem::swap(into, &mut from);
    }
    for (k, (s, u)) in from {
        if into.insert(k, (s, u)).is_some() {
            *size -= s;
            *usage -= u;
        }
    }
}

/// Directory-specific data, boxed so regular files stay 64 bytes (one cache line).
#[derive(Debug, Clone)]
pub struct DirExt {
    pub children: Vec<Item>,
    /// Raw item count (a dir counts itself, a file is 1).
    pub items: i64,
    /// Blocks of the directory itself (in bytes).
    pub own_usage: i64,
    /// Stats came from a JSON import; do not recompute.
    pub fixed_stats: bool,
    /// Device of this directory (folded to 32 bits, see `fold_dev`). Files inside live on the
    /// same device, so hardlink identity is `(dir dev, ino)` and no per-file device is stored.
    pub dev: u32,
}

impl Default for DirExt {
    #[inline]
    fn default() -> Self {
        Self {
            children: Vec::new(),
            items: 1,
            own_usage: 0,
            fixed_stats: false,
            dev: 0,
        }
    }
}

/// 16-byte payload distinguishing files from directories.
/// `flag`: ' ' normal, '!' read error, '.' partial, '@' non-regular, 'H' hardlink, 'e' empty dir
#[derive(Debug, Clone)]
pub enum Kind {
    File { ino: u64, flag: char },
    Dir { ext: Box<DirExt>, flag: char },
}

impl Default for Kind {
    #[inline]
    fn default() -> Self {
        Self::File { ino: 0, flag: ' ' }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Item {
    pub name: SmallStr,
    pub size: i64,
    pub usage: i64,
    pub mtime: i64,
    pub kind: Kind,
}

// Guarantee every Item fits in a single 64-byte CPU cache line.
const _: [(); 64] = [(); std::mem::size_of::<Item>()];

#[derive(Clone, Copy, PartialEq)]
pub enum SortBy {
    Size,
    Apparent,
    Name,
    Count,
    Mtime,
}

#[inline]
fn cmp_ignore_case(a: &str, b: &str) -> Ordering {
    if a.is_ascii() && b.is_ascii() {
        a.bytes()
            .map(|c| c.to_ascii_lowercase())
            .cmp(b.bytes().map(|c| c.to_ascii_lowercase()))
    } else {
        a.chars()
            .flat_map(char::to_lowercase)
            .cmp(b.chars().flat_map(char::to_lowercase))
    }
}

impl Item {
    #[inline]
    pub fn new_dir(name: impl Into<SmallStr>, flag: char) -> Self {
        Self {
            name: name.into(),
            size: 0,
            usage: 0,
            mtime: 0,
            kind: Kind::Dir {
                ext: Box::new(DirExt::default()),
                flag,
            },
        }
    }

    #[inline]
    pub fn new_file(
        name: impl Into<SmallStr>,
        flag: char,
        size: i64,
        usage: i64,
        mtime: i64,
        ino: u64,
    ) -> Self {
        Self {
            name: name.into(),
            size,
            usage,
            mtime,
            kind: Kind::File { ino, flag },
        }
    }

    #[inline]
    pub fn is_dir(&self) -> bool {
        matches!(self.kind, Kind::Dir { .. })
    }

    #[inline]
    pub fn flag(&self) -> char {
        match &self.kind {
            Kind::File { flag, .. } | Kind::Dir { flag, .. } => *flag,
        }
    }

    #[inline]
    pub fn ino(&self) -> u64 {
        match &self.kind {
            Kind::File { ino, .. } => *ino,
            Kind::Dir { .. } => 0,
        }
    }

    #[inline]
    pub fn items(&self) -> i64 {
        match &self.kind {
            Kind::File { .. } => 1,
            Kind::Dir { ext, .. } => ext.items,
        }
    }

    #[inline]
    pub fn children(&self) -> &[Item] {
        match &self.kind {
            Kind::File { .. } => &[],
            Kind::Dir { ext, .. } => &ext.children,
        }
    }

    #[inline]
    pub fn children_mut(&mut self) -> &mut Vec<Item> {
        match &mut self.kind {
            Kind::Dir { ext, .. } => &mut ext.children,
            Kind::File { .. } => unreachable!("children_mut called on file"),
        }
    }

    #[inline]
    pub fn dir_ext(&self) -> Option<&DirExt> {
        match &self.kind {
            Kind::Dir { ext, .. } => Some(ext),
            Kind::File { .. } => None,
        }
    }

    #[inline]
    pub fn dir_ext_mut(&mut self) -> Option<&mut DirExt> {
        match &mut self.kind {
            Kind::Dir { ext, .. } => Some(ext),
            Kind::File { .. } => None,
        }
    }

    /// Count shown to the user: dirs do not count themselves.
    #[inline]
    pub fn display_count(&self) -> i64 {
        match &self.kind {
            Kind::Dir { ext, .. } => (ext.items - 1).max(0),
            Kind::File { .. } => 1,
        }
    }

    /// Recompute dir totals bottom-up. Every directory counts a hardlinked file (`(dir dev, ino)`)
    /// once within its own subtree; returns the links found below so the parent can do the same.
    pub fn update_stats(&mut self) -> LinkMap<(u32, u64)> {
        let Kind::Dir { ext, flag } = &mut self.kind else {
            return LinkMap::default();
        };
        if ext.fixed_stats {
            return LinkMap::default();
        }
        let (mut size, mut usage, mut items) = (0, 0, 1);
        let mut links = LinkMap::default();
        let mut max_mtime = self.mtime;
        let dev = ext.dev;
        for c in &mut ext.children {
            let child_links = c.update_stats();
            size += c.size;
            usage += c.usage;
            merge_links(&mut size, &mut usage, &mut links, child_links);
            let ino = c.ino();
            if ino != 0 && links.insert((dev, ino), (c.size, c.usage)).is_some() {
                size -= c.size;
                usage -= c.usage;
            }
            items += c.items();
            max_mtime = max_mtime.max(c.mtime);
            let cf = c.flag();
            if (cf == '!' || cf == '.') && *flag != '!' {
                *flag = '.';
            }
        }
        if ext.children.is_empty() {
            size += EMPTY_DIR_SIZE;
            if *flag == ' ' {
                *flag = 'e';
            }
        }
        self.size = size;
        self.usage = usage + ext.own_usage;
        self.mtime = max_mtime;
        ext.items = items;
        links
    }

    pub fn sort(&mut self, by: SortBy, desc: bool) {
        let Some(ext) = self.dir_ext_mut() else {
            return;
        };
        ext.children.sort_unstable_by(|a, b| {
            let o = match by {
                SortBy::Size => a.usage.cmp(&b.usage),
                SortBy::Apparent => a.size.cmp(&b.size),
                SortBy::Count => a.display_count().cmp(&b.display_count()),
                SortBy::Mtime => a.mtime.cmp(&b.mtime),
                SortBy::Name => cmp_ignore_case(&a.name, &b.name),
            };
            // ties fall back to name so output is stable
            let o = o.then_with(|| b.name.cmp(&a.name));
            if desc { o.reverse() } else { o }
        });
    }

    /// Remove child `idx` of the dir at `path`, then recompute every total from the tree.
    /// Recomputing (instead of subtracting) keeps hardlinks, empty directories and flags exact.
    pub fn delete_child(&mut self, path: &[usize], idx: usize) {
        self.dir_at(path).children_mut().remove(idx);
        self.update_stats();
    }

    /// Real filesystem path of child `idx` of the dir at `path` (self is the scan root).
    pub fn path_of(&self, path: &[usize], idx: usize) -> PathBuf {
        let mut p = PathBuf::from(&self.name);
        let mut d = self;
        for &i in path {
            d = &d.children()[i];
            p.push(&d.name);
        }
        p.push(&d.children()[idx].name);
        p
    }

    pub fn dir_at(&mut self, path: &[usize]) -> &mut Item {
        path.iter().fold(self, |d, &i| &mut d.children_mut()[i])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str, ino: u64) -> Item {
        Item::new_file(name, if ino != 0 { 'H' } else { ' ' }, 100, 4096, 1, ino)
    }

    fn dir(name: &str, dev: u32, kids: Vec<Item>) -> Item {
        let mut d = Item::new_dir(name, ' ');
        d.dir_ext_mut().unwrap().dev = dev;
        *d.children_mut() = kids;
        d
    }

    #[test]
    fn raw_name_keeps_bytes() {
        let n = SmallStr::from_utf8_lossy(b"bad\xffname");
        assert_eq!(n.as_os_str().as_bytes(), b"bad\xffname");
        assert_eq!(n.as_str(), "bad\u{fffd}name");
        assert_eq!(SmallStr::from_utf8_lossy(b"ok").as_os_str(), "ok");
    }

    #[test]
    fn item_stays_one_cache_line() {
        assert_eq!(std::mem::size_of::<Item>(), 64);
    }

    #[test]
    fn deleting_one_hardlink_name_keeps_the_size() {
        let a = dir("a", 1, vec![file("f1", 5), file("f2", 5)]);
        let mut root = dir("r", 1, vec![a]);
        root.update_stats();
        assert_eq!(root.usage, 4096);
        root.delete_child(&[0], 0); // remove f1; f2 still occupies the inode
        assert_eq!((root.children()[0].usage, root.usage), (4096, 4096));
        root.delete_child(&[0], 0); // remove f2: the directory is empty now
        let a = &root.children()[0];
        assert_eq!((a.size, a.usage, a.flag()), (EMPTY_DIR_SIZE, 0, 'e'));
        assert_eq!(root.size, EMPTY_DIR_SIZE);
    }

    #[test]
    fn same_inode_on_different_devices_is_two_files() {
        let mut root = dir(
            "r",
            0,
            vec![
                dir("a", 1, vec![file("x", 5)]),
                dir("b", 2, vec![file("y", 5)]),
            ],
        );
        root.update_stats();
        assert_eq!(root.usage, 8192);
        let mut same_dev = dir(
            "r",
            0,
            vec![
                dir("a", 3, vec![file("x", 5)]),
                dir("b", 3, vec![file("y", 5)]),
            ],
        );
        same_dev.update_stats();
        assert_eq!(same_dev.usage, 4096);
    }
}
