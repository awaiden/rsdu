use crate::fmt::{self, Fmt};
use crate::scan::Progress;
use crate::tree::{Item, SortBy};
use ratatui::Frame;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, List, ListItem, ListState, Paragraph};
use std::io;
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub struct Cfg {
    pub fmt: Fmt,
    pub apparent: bool,
    pub show_count: bool,
    pub show_mtime: bool,
    pub no_color: bool,
    pub no_unicode: bool,
    pub no_delete: bool,
}

pub enum Source {
    Ready(Item),
    /// Scan runs on another thread; `String` is the scanned path.
    Scanning(JoinHandle<Item>, Arc<Progress>, String),
}

struct Scanning {
    handle: JoinHandle<Item>,
    prog: Arc<Progress>,
    path: String,
    started: Instant,
}

struct Deleting {
    name: String,
    size: i64,
    idx: usize,
    path: Vec<usize>,
    handle: JoinHandle<io::Result<()>>,
    started: Instant,
}

struct App {
    root: Item,
    path: Vec<usize>,
    sel: ListState,
    cfg: Cfg,
    by: SortBy,
    desc: bool,
    confirm: bool,
    help: bool,
    scanning: Option<Scanning>,
    deleting: Option<Deleting>,
    /// (text, is_error)
    status: Option<(String, bool)>,
    /// Something changed since the last draw (idle browsing needs no redraw).
    dirty: bool,
}

fn cur<'a>(root: &'a Item, path: &[usize]) -> &'a Item {
    path.iter().fold(root, |d, &i| &d.children()[i])
}

fn mtime_str(t: i64) -> String {
    // civil date from unix days (UTC)
    let z = t.div_euclid(86400) + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Keep the tail of a long path so the deepest dir stays visible.
fn tail(s: &str, w: usize) -> String {
    let n = s.chars().count();
    if n <= w {
        return s.to_string();
    }
    let keep = w.saturating_sub(1);
    format!("…{}", s.chars().skip(n - keep).collect::<String>())
}

pub fn run(src: Source, cfg: Cfg) -> io::Result<()> {
    let mut app = App {
        root: Item::default(),
        path: vec![],
        sel: ListState::default(),
        cfg,
        by: SortBy::Size,
        desc: true,
        confirm: false,
        help: false,
        scanning: None,
        deleting: None,
        status: None,
        dirty: true,
    };
    match src {
        Source::Ready(root) => app.set_root(root),
        Source::Scanning(handle, prog, path) => {
            app.scanning = Some(Scanning {
                handle,
                prog,
                path,
                started: Instant::now(),
            })
        }
    }
    let mut term = ratatui::init();
    let res = (|| loop {
        app.tick();
        // scanning and deleting animate (spinner, counters); otherwise draw only after a change
        if app.dirty || app.scanning.is_some() || app.deleting.is_some() {
            term.draw(|f| app.draw(f))?;
            app.dirty = false;
        }
        if !event::poll(Duration::from_millis(100))? {
            continue;
        }
        let k = match event::read()? {
            Event::Key(k) => k,
            Event::Resize(..) => {
                app.dirty = true;
                continue;
            }
            _ => continue,
        };
        if k.kind != KeyEventKind::Press {
            continue;
        }
        app.dirty = true;
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
            return Ok(());
        }
        if app.key(k.code) {
            return Ok(());
        }
    })();
    ratatui::restore();
    res
}

impl App {
    fn set_root(&mut self, root: Item) {
        self.root = root;
        self.resort();
        self.sel.select(Some(0));
    }

    fn st(&self, s: Style) -> Style {
        if self.cfg.no_color {
            Style::default()
        } else {
            s
        }
    }

    fn bar_style(&self) -> Style {
        if self.cfg.no_color {
            Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD)
        } else {
            Style::default()
                .fg(Color::White)
                .bg(Color::Blue)
                .add_modifier(Modifier::BOLD)
        }
    }

    fn spinner(&self, since: Instant) -> char {
        let frames: &[char] = if self.cfg.no_unicode {
            &['|', '/', '-', '\\']
        } else {
            &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏']
        };
        frames[(since.elapsed().as_millis() / 100) as usize % frames.len()]
    }

    fn metric(&self, i: &Item) -> i64 {
        if self.cfg.apparent { i.size } else { i.usage }
    }

    /// Sort only the currently viewed directory on demand.
    fn resort(&mut self) {
        let by = match self.by {
            SortBy::Size if self.cfg.apparent => SortBy::Apparent,
            b => b,
        };
        self.root.dir_at(&self.path).sort(by, self.desc);
    }

    fn set_sort(&mut self, by: SortBy) {
        self.desc = if self.by == by {
            !self.desc
        } else {
            by != SortBy::Name
        };
        self.by = by;
        self.resort();
    }

    /// Pick up finished background work (scan or delete).
    fn tick(&mut self) {
        if self
            .scanning
            .as_ref()
            .is_some_and(|s| s.handle.is_finished())
        {
            let s = self.scanning.take().unwrap();
            match s.handle.join() {
                Ok(root) => self.set_root(root),
                Err(_) => self.status = Some(("Scan thread panicked".into(), true)),
            }
            self.dirty = true;
        }
        if self
            .deleting
            .as_ref()
            .is_some_and(|d| d.handle.is_finished())
        {
            let d = self.deleting.take().unwrap();
            self.status = Some(match d.handle.join() {
                Ok(Ok(())) => {
                    self.root.delete_child(&d.path, d.idx);
                    // totals changed, so sibling order may have too
                    self.resort();
                    let n = cur(&self.root, &self.path).children().len();
                    self.sel
                        .select(if n == 0 { None } else { Some(d.idx.min(n - 1)) });
                    (
                        format!("Deleted {} (freed {})", d.name, self.cfg.fmt.size(d.size)),
                        false,
                    )
                }
                Ok(Err(e)) => (format!("Error deleting {}: {e}", d.name), true),
                Err(_) => (format!("Delete of {} panicked", d.name), true),
            });
            self.dirty = true;
        }
    }

    /// Returns true to quit.
    fn key(&mut self, code: KeyCode) -> bool {
        if self.scanning.is_some() {
            return matches!(code, KeyCode::Char('q' | 'Q') | KeyCode::Esc);
        }
        if self.deleting.is_some() {
            return false; // busy: ignore keys until the delete finishes
        }
        self.status = None;
        if self.help {
            self.help = false;
            return false;
        }
        if self.confirm {
            self.confirm = false;
            if matches!(code, KeyCode::Char('y')) {
                self.start_delete();
            }
            return false;
        }
        let n = cur(&self.root, &self.path).children().len();
        let i = self.sel.selected().unwrap_or(0);
        match code {
            KeyCode::Char('q' | 'Q') => return true,
            KeyCode::Char('?') => self.help = true,
            KeyCode::Down | KeyCode::Char('j') => {
                self.sel.select(Some((i + 1).min(n.saturating_sub(1))))
            }
            KeyCode::Up | KeyCode::Char('k') => self.sel.select(Some(i.saturating_sub(1))),
            KeyCode::Home | KeyCode::Char('g') => self.sel.select(Some(0)),
            KeyCode::End | KeyCode::Char('G') => self.sel.select(Some(n.saturating_sub(1))),
            KeyCode::Right | KeyCode::Enter | KeyCode::Char('l') => {
                if cur(&self.root, &self.path)
                    .children()
                    .get(i)
                    .is_some_and(Item::is_dir)
                {
                    self.path.push(i);
                    self.resort();
                    self.sel.select(Some(0));
                }
            }
            KeyCode::Left | KeyCode::Backspace | KeyCode::Char('h') => {
                if let Some(p) = self.path.pop() {
                    let prev_name = self.root.dir_at(&self.path).children()[p].name.clone();
                    self.resort();
                    let new_idx = cur(&self.root, &self.path)
                        .children()
                        .iter()
                        .position(|c| c.name == prev_name)
                        .unwrap_or(p);
                    self.sel.select(Some(new_idx));
                }
            }
            KeyCode::Char('a') => {
                self.cfg.apparent = !self.cfg.apparent;
                self.resort();
            }
            KeyCode::Char('c') => self.cfg.show_count = !self.cfg.show_count,
            KeyCode::Char('m') => self.cfg.show_mtime = !self.cfg.show_mtime,
            KeyCode::Char('s') => self.set_sort(SortBy::Size),
            KeyCode::Char('C') => self.set_sort(SortBy::Count),
            KeyCode::Char('n') => self.set_sort(SortBy::Name),
            KeyCode::Char('M') => self.set_sort(SortBy::Mtime),
            KeyCode::Char('d') => {
                if self.cfg.no_delete {
                    self.status = Some((
                        "Deletion is disabled (--no-delete or imported file)".into(),
                        true,
                    ));
                } else if n > 0 {
                    self.confirm = true;
                }
            }
            _ => {}
        }
        false
    }

    fn start_delete(&mut self) {
        let Some(idx) = self.sel.selected() else {
            return;
        };
        let p = self.root.path_of(&self.path, idx);
        let item = &cur(&self.root, &self.path).children()[idx];
        let (name, size) = (item.name.to_string(), self.metric(item));
        let handle = std::thread::spawn(move || {
            if std::fs::symlink_metadata(&p)?.file_type().is_dir() {
                std::fs::remove_dir_all(&p)
            } else {
                std::fs::remove_file(&p)
            }
        });
        self.deleting = Some(Deleting {
            name,
            size,
            idx,
            path: self.path.clone(),
            handle,
            started: Instant::now(),
        });
    }

    fn draw(&mut self, f: &mut Frame) {
        let [head, body, foot] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(2),
        ])
        .areas(f.area());
        let bar = self.bar_style();

        if let Some(s) = &self.scanning {
            f.render_widget(Paragraph::new(" rsdu").style(bar), head);
            let p = &s.prog;
            let lines = vec![
                Line::from(format!(
                    "{} Scanning {}",
                    self.spinner(s.started),
                    tail(&s.path, 50)
                )),
                Line::from(""),
                Line::from(format!(
                    "Items:    {}",
                    fmt::count(p.items.load(Relaxed) as i64)
                )),
                Line::from(format!(
                    "Size:     {}",
                    self.cfg.fmt.size(p.bytes.load(Relaxed) as i64)
                )),
                Line::from(format!("Elapsed:  {}s", s.started.elapsed().as_secs())),
                Line::from(""),
                Line::from(format!(
                    "Current:  {}",
                    tail(&p.current.lock().unwrap(), 48)
                )),
                Line::from(""),
                Line::from("q / Esc to quit")
                    .style(self.st(Style::default().add_modifier(Modifier::DIM))),
            ];
            popup(
                f,
                64,
                lines.len() as u16 + 2,
                " Scanning ",
                self.st(Style::default().fg(Color::Cyan)),
                lines,
            );
            return;
        }

        let dir = cur(&self.root, &self.path);
        let mut shown = std::path::PathBuf::from(&self.root.name);
        let mut d = &self.root;
        for &i in &self.path {
            d = &d.children()[i];
            shown.push(&d.name);
        }
        f.render_widget(
            Paragraph::new(format!(" rsdu  {}", shown.display())).style(bar),
            head,
        );

        let children = dir.children();
        let max = children
            .iter()
            .map(|c| self.metric(c))
            .max()
            .unwrap_or(0)
            .max(1);

        // Compute viewport window so we only build ListItems for visible rows
        let height = body.height as usize;
        let total = children.len();
        let selected = self
            .sel
            .selected()
            .unwrap_or(0)
            .min(total.saturating_sub(1));
        let mut offset = self.sel.offset();
        if selected < offset {
            offset = selected;
        } else if height > 0 && selected >= offset + height {
            offset = selected + 1 - height;
        }
        *self.sel.offset_mut() = offset;
        let end = (offset + height).min(total);

        const UNI_FULL: &str = "██████████";
        const UNI_EMPTY: &str = "░░░░░░░░░░";
        const ASC_FULL: &str = "##########";
        const ASC_EMPTY: &str = "          ";

        let rows: Vec<ListItem> = children[offset..end]
            .iter()
            .map(|c| {
                let m = self.metric(c);
                let filled = (m * 10 / max).clamp(0, 10) as usize;
                let (on, off) = if self.cfg.no_unicode {
                    (&ASC_FULL[..filled], &ASC_EMPTY[..10 - filled])
                } else {
                    (&UNI_FULL[..filled * 3], &UNI_EMPTY[..(10 - filled) * 3])
                };
                let mut spans = Vec::with_capacity(9);
                spans.push(Span::styled(
                    format!("{} ", c.flag()),
                    self.st(Style::default().fg(Color::DarkGray)),
                ));
                spans.push(Span::raw(format!("{:>10} ", self.cfg.fmt.size(m))));
                spans.push(Span::raw("["));
                spans.push(Span::styled(on, self.st(Style::default().fg(Color::Green))));
                spans.push(Span::styled(
                    off,
                    self.st(Style::default().fg(Color::DarkGray)),
                ));
                spans.push(Span::raw("] "));
                if self.cfg.show_count {
                    spans.push(Span::raw(format!("{:>6} ", fmt::count(c.display_count()))));
                }
                if self.cfg.show_mtime {
                    spans.push(Span::raw(format!("{} ", mtime_str(c.mtime))));
                }
                if c.is_dir() {
                    spans.push(Span::styled(
                        format!("/{}", c.name),
                        Style::default().add_modifier(Modifier::BOLD),
                    ));
                } else {
                    spans.push(Span::raw(c.name.as_str()));
                }
                ListItem::new(Line::from(spans))
            })
            .collect();
        let hl = if self.cfg.no_color {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default()
                .fg(Color::White)
                .bg(Color::Blue)
                .add_modifier(Modifier::BOLD)
        };
        let mut view_state = ListState::default();
        if total > 0 {
            view_state.select(Some(selected - offset));
        }
        f.render_stateful_widget(List::new(rows).highlight_style(hl), body, &mut view_state);

        let totals = format!(
            " Total disk usage: {}  Apparent size: {}  Items: {}",
            self.cfg.fmt.size(dir.usage),
            self.cfg.fmt.size(dir.size),
            dir.display_count()
        );
        let [l1, l2] = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(foot);
        f.render_widget(Paragraph::new(totals).style(bar), l1);
        let line = match &self.status {
            Some((t, true)) => Line::from(format!(" {t}"))
                .style(self.st(Style::default().fg(Color::Red).add_modifier(Modifier::BOLD))),
            Some((t, false)) => {
                Line::from(format!(" {t}")).style(self.st(Style::default().fg(Color::Green)))
            }
            None => Line::from(" ?: help   q: quit"),
        };
        f.render_widget(Paragraph::new(line), l2);

        if self.confirm {
            if let Some(c) = self.sel.selected().and_then(|i| children.get(i)) {
                let label = if c.is_dir() {
                    format!("/{}", c.name)
                } else {
                    c.name.to_string()
                };
                let lines = vec![
                    Line::from(format!(
                        "Delete {label} ({})?",
                        self.cfg.fmt.size(self.metric(c))
                    )),
                    Line::from(""),
                    Line::from("y: delete   any other key: cancel"),
                ];
                popup(
                    f,
                    56,
                    5,
                    " Delete ",
                    self.st(Style::default().fg(Color::Red)),
                    lines,
                );
            }
        }
        if let Some(d) = &self.deleting {
            let lines = vec![
                Line::from(format!(
                    "{} Deleting {} ({})…",
                    self.spinner(d.started),
                    d.name,
                    self.cfg.fmt.size(d.size)
                )),
                Line::from(""),
                Line::from(format!("Elapsed: {}s", d.started.elapsed().as_secs())),
            ];
            popup(
                f,
                56,
                5,
                " Deleting ",
                self.st(Style::default().fg(Color::Yellow)),
                lines,
            );
        }
        if self.help {
            let k = |a: &str, b: &str| Line::from(format!("  {a:<18}{b}"));
            let lines = vec![
                k("j/k ↑/↓", "move"),
                k("l/→/Enter", "open directory"),
                k("h/←/Backspace", "parent directory"),
                k("g/G", "first / last"),
                k("a", "apparent size / disk usage"),
                k("c  m", "item count / mtime column"),
                k("s C n M", "sort by size/count/name/mtime"),
                k("", "(again = reverse)"),
                k("d", "delete"),
                k("q", "quit"),
                Line::from(""),
                Line::from("  press any key to close"),
            ];
            popup(
                f,
                56,
                lines.len() as u16 + 2,
                " Help ",
                self.st(Style::default().fg(Color::Cyan)),
                lines,
            );
        }
    }
}

fn popup(f: &mut Frame, w: u16, h: u16, title: &str, border: Style, lines: Vec<Line>) {
    let a = f.area();
    let r = Rect {
        x: a.x + a.width.saturating_sub(w) / 2,
        y: a.y + a.height.saturating_sub(h) / 2,
        width: w.min(a.width),
        height: h.min(a.height),
    };
    f.render_widget(Clear, r);
    f.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(title).border_style(border)),
        r,
    );
}
