use crate::fmt::{self, Fmt};
use crate::tree::{Item, SortBy};
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::io::Write;

pub struct Cfg {
    pub fmt: Fmt,
    pub apparent: bool,
    pub show_count: bool,
    pub color: bool,
    pub reverse: bool,
    pub top: usize,
    pub depth: usize,
    pub summarize: bool,
}

fn paint(on: bool, code: &str, s: String) -> String {
    if on {
        format!("\x1b[{code}m{s}\x1b[0m")
    } else {
        s
    }
}

struct TopEntry<'a> {
    metric: i64,
    seq: usize,
    path: String,
    item: &'a Item,
}

impl PartialEq for TopEntry<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.metric == other.metric && self.seq == other.seq
    }
}

impl Eq for TopEntry<'_> {}

impl PartialOrd for TopEntry<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for TopEntry<'_> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Max-heap on (Reverse(metric), seq) so the smallest metric (or latest tie) is at peek()
        Reverse(self.metric)
            .cmp(&Reverse(other.metric))
            .then_with(|| self.seq.cmp(&other.seq))
    }
}

pub fn print(root: &mut Item, scan_path: &str, c: &Cfg, w: &mut impl Write) -> std::io::Result<()> {
    let metric = |i: &Item| if c.apparent { i.size } else { i.usage };
    let size = |i: &Item| paint(c.color, "33", format!("{:>9}", c.fmt.size(metric(i))));
    let by = if c.apparent {
        SortBy::Apparent
    } else {
        SortBy::Size
    };
    let path_line = |w: &mut dyn Write, i: &Item, p: &str| {
        let p = if i.is_dir() {
            paint(c.color, "34", p.to_string())
        } else {
            p.to_string()
        };
        writeln!(w, "{} {p}", size(i))
    };

    if c.summarize {
        let base = std::path::Path::new(scan_path)
            .file_name()
            .map_or(scan_path.to_string(), |n| n.to_string_lossy().into());
        return writeln!(w, "{} {base}", size(root));
    }
    if c.top > 0 {
        let mut heap = BinaryHeap::with_capacity(c.top);
        let mut seq = 0;
        collect_top(
            root,
            scan_path.trim_end_matches('/'),
            c.top,
            &metric,
            &mut seq,
            &mut heap,
        );
        let mut top_items = heap.into_vec();
        top_items.sort_unstable_by_key(|e| (Reverse(e.metric), e.seq));
        for e in top_items {
            path_line(w, e.item, &e.path)?;
        }
        return Ok(());
    }
    if c.depth > 0 {
        return walk(root, scan_path, 0, c, by, &path_line, w);
    }
    root.sort(by, !c.reverse);
    for f in root.children() {
        let name = if f.is_dir() {
            paint(c.color, "34", format!("/{}", f.name))
        } else {
            f.name.to_string()
        };
        let cnt = if c.show_count {
            format!(
                " {:>11}",
                paint(c.color, "31", fmt::count(f.display_count()))
            )
        } else {
            String::new()
        };
        writeln!(w, "{} {}{cnt} {name}", f.flag(), size(f))?;
    }
    Ok(())
}

fn collect_top<'a>(
    d: &'a Item,
    prefix: &str,
    top: usize,
    metric: &impl Fn(&Item) -> i64,
    seq: &mut usize,
    heap: &mut BinaryHeap<TopEntry<'a>>,
) {
    for ch in d.children() {
        if ch.is_dir() {
            let p = format!("{prefix}/{}", ch.name);
            collect_top(ch, &p, top, metric, seq, heap);
        } else {
            let m = metric(ch);
            *seq += 1;
            if heap.len() < top {
                heap.push(TopEntry {
                    metric: m,
                    seq: *seq,
                    path: format!("{prefix}/{}", ch.name),
                    item: ch,
                });
            } else if let Some(min) = heap.peek()
                && m > min.metric
            {
                heap.pop();
                heap.push(TopEntry {
                    metric: m,
                    seq: *seq,
                    path: format!("{prefix}/{}", ch.name),
                    item: ch,
                });
            }
        }
    }
}

fn walk(
    d: &mut Item,
    path: &str,
    level: usize,
    c: &Cfg,
    by: SortBy,
    line: &dyn Fn(&mut dyn Write, &Item, &str) -> std::io::Result<()>,
    w: &mut impl Write,
) -> std::io::Result<()> {
    line(w, d, path)?;
    if level < c.depth {
        d.sort(by, !c.reverse);
        let prefix = path.trim_end_matches('/');
        for ch in d.children_mut() {
            let p = format!("{prefix}/{}", ch.name);
            if ch.is_dir() {
                walk(ch, &p, level + 1, c, by, line, w)?;
            } else {
                line(w, ch, &p)?;
            }
        }
    }
    Ok(())
}
