mod devices;
mod fmt;
mod ignore;
mod json;
mod scan;
mod stdout;
mod tree;
mod tui;

use clap::Parser;
use fmt::Fmt;
use std::io::{BufWriter, IsTerminal, Write};
use std::path::PathBuf;
use std::sync::atomic::Ordering::Relaxed;
use std::time::Duration;

#[derive(Parser)]
#[command(version, about = "Fast disk usage analyzer, a Rust port of ncdu")]
struct Args {
    /// Directory to analyze (default: current directory)
    path: Option<String>,
    #[arg(
        short = 'o',
        long,
        help = "Export all info into file as JSON (- for stdout)"
    )]
    output_file: Option<String>,
    #[arg(
        long,
        value_delimiter = ',',
        help = "Export only selected JSON attributes (name,asize,dsize,shsize,items,mtime,notreg,ino)"
    )]
    output_attrs: Vec<String>,
    #[arg(short = 'f', long, help = "Import analysis from JSON file")]
    input_file: Option<String>,
    #[arg(short = 'm', long, help = "Max cores to use")]
    max_cores: Option<usize>,
    #[arg(long, help = "Use sequential scanning (for rotating HDDs)")]
    sequential: bool,
    #[arg(short = 'i', long, value_delimiter = ',', default_values_t = ["/proc", "/dev", "/sys", "/run"].map(String::from), help = "Absolute paths of directories to ignore")]
    ignore_dirs: Vec<String>,
    #[arg(
        short = 'I',
        long,
        value_delimiter = ',',
        help = "Regex patterns of paths to ignore"
    )]
    ignore_dirs_pattern: Vec<String>,
    #[arg(
        short = 'X',
        long,
        help = "File with regex patterns to ignore, one per line"
    )]
    ignore_from: Option<String>,
    #[arg(short = 'H', long, help = "Ignore hidden files and directories")]
    no_hidden: bool,
    #[arg(short = 'x', long, help = "Do not cross filesystem boundaries")]
    no_cross: bool,
    #[arg(short = 'd', long, help = "Show all mounted disks")]
    show_disks: bool,
    #[arg(short = 'a', long, help = "Show apparent size")]
    show_apparent_size: bool,
    #[arg(short = 'c', long, help = "Do not use colors")]
    no_color: bool,
    #[arg(short = 'C', long, help = "Show number of items in directory")]
    show_item_count: bool,
    #[arg(short = 'M', long, help = "Show latest mtime of items (TUI)")]
    show_mtime: bool,
    #[arg(short = 'n', long, help = "Do not run in interactive mode")]
    non_interactive: bool,
    #[arg(long, help = "Force interactive mode even when output is not a TTY")]
    interactive: bool,
    #[arg(short = 'p', long, help = "Do not show progress")]
    no_progress: bool,
    #[arg(short = 'u', long, help = "Do not use Unicode symbols")]
    no_unicode: bool,
    #[arg(short = 's', long, help = "Show only a total (non-interactive)")]
    summarize: bool,
    #[arg(
        short = 't',
        long,
        default_value_t = 0,
        help = "Show only top X largest files (non-interactive)"
    )]
    top: usize,
    #[arg(
        long,
        default_value_t = 0,
        help = "Show directory tree up to this depth (non-interactive)"
    )]
    depth: usize,
    #[arg(long, help = "Decimal SI prefixes (kB, MB) instead of binary")]
    si: bool,
    #[arg(long, help = "Raw sizes without prefixes (non-interactive)")]
    no_prefix: bool,
    #[arg(short = 'k', long, help = "Show sizes in KiB (non-interactive)")]
    show_in_kib: bool,
    #[arg(long, help = "Reverse sort order (non-interactive)")]
    reverse_sort: bool,
    #[arg(long, help = "Do not allow deletions")]
    no_delete: bool,
}

fn die(msg: impl std::fmt::Display) -> ! {
    eprintln!("rsdu: {msg}");
    std::process::exit(1)
}

fn main() {
    let a = Args::parse();
    let fmt = Fmt {
        si: a.si,
        no_prefix: a.no_prefix,
        kib: a.show_in_kib,
    };
    let tty = std::io::stdout().is_terminal();
    let color = !a.no_color && tty;
    let out = &mut std::io::stdout().lock();

    if a.show_disks {
        let devs = devices::list().unwrap_or_else(|e| die(e));
        let w = devs.iter().map(|d| d.name.len()).max().unwrap_or(0).max(6);
        let _ = writeln!(
            out,
            "{:>w$} {:>9} {:>9} {:>9} {:>5} Mount point",
            "Device", "Size", "Used", "Free", "Used%"
        );
        for d in devs {
            let used = d.size - d.free;
            let pct = if d.size > 0 {
                (used as f64 / d.size as f64 * 100.0).round()
            } else {
                0.0
            };
            let _ = writeln!(
                out,
                "{:>w$} {:>9} {:>9} {:>9} {:>4.0}% {}",
                d.name,
                fmt.size(d.size),
                fmt.size(used),
                fmt.size(d.free),
                pct,
                d.mount
            );
        }
        return;
    }

    let cores = if a.sequential {
        1
    } else {
        a.max_cores.unwrap_or(0)
    };
    rayon::ThreadPoolBuilder::new()
        .num_threads(cores)
        .build_global()
        .ok();

    let path = PathBuf::from(a.path.clone().unwrap_or_else(|| ".".into()));
    let abs = std::path::absolute(&path).unwrap_or_else(|e| die(e));
    let scan_path = abs.to_string_lossy().trim_end_matches('/').to_string();
    let scan_path = if scan_path.is_empty() {
        "/".into()
    } else {
        scan_path
    };

    let batch =
        a.non_interactive || a.summarize || a.top > 0 || a.depth > 0 || (!tty && !a.interactive);
    let interactive = !batch && a.output_file.is_none();
    let tui_cfg = || tui::Cfg {
        fmt: Fmt {
            si: a.si,
            ..Default::default()
        },
        apparent: a.show_apparent_size,
        show_count: a.show_item_count,
        show_mtime: a.show_mtime,
        no_color: a.no_color,
        no_unicode: a.no_unicode,
        no_delete: a.no_delete,
    };

    let mut root = if let Some(f) = &a.input_file {
        let text = if f == "-" {
            std::io::read_to_string(std::io::stdin())
        } else {
            std::fs::read_to_string(f)
        };
        json::import(&text.unwrap_or_else(|e| die(e))).unwrap_or_else(|e| die(e))
    } else {
        if let Ok(m) = abs.metadata()
            && m.is_file()
        {
            let it = scan::file_item(abs.file_name().unwrap().to_string_lossy().into(), &m);
            let s = if a.show_apparent_size {
                it.size
            } else {
                it.usage
            };
            let _ = writeln!(out, "{:>9} {}", fmt.size(s), it.name);
            return;
        }
        let mut pats = a.ignore_dirs_pattern.clone();
        if let Some(f) = &a.ignore_from {
            pats.extend(ignore::read_patterns(f).unwrap_or_else(|e| die(e)));
        }
        let max_depth = if interactive || a.output_file.is_some() || a.top > 0 {
            None
        } else if a.summarize {
            Some(0)
        } else if a.depth > 0 {
            Some(a.depth)
        } else {
            Some(1)
        };
        let opts = scan::Opts {
            ignore: ignore::Ignore::new(&a.ignore_dirs, &pats, a.no_hidden)
                .unwrap_or_else(|e| die(e)),
            no_cross: a.no_cross,
            max_depth,
        };
        if interactive {
            // scan on a worker thread so the TUI can show live progress
            let prog = std::sync::Arc::new(scan::Progress::active());
            let p = prog.clone();
            let handle = std::thread::spawn(move || scan::scan(&abs, &opts, &p));
            tui::run(tui::Source::Scanning(handle, prog, scan_path), tui_cfg())
                .unwrap_or_else(|e| die(e));
            return;
        }
        let show = !a.no_progress && std::io::stderr().is_terminal();
        let prog = scan::Progress {
            active: show,
            ..Default::default()
        };
        let done = std::sync::atomic::AtomicBool::new(false);

        std::thread::scope(|s| {
            if show {
                s.spawn(|| {
                    while !done.load(Relaxed) {
                        eprint!(
                            "\r\x1b[K{} items, {}  {}",
                            prog.items.load(Relaxed),
                            fmt.size(prog.bytes.load(Relaxed) as i64),
                            prog.current.lock().unwrap()
                        );
                        std::thread::sleep(Duration::from_millis(100));
                    }
                    eprint!("\r\x1b[K");
                });
            }
            let r = scan::scan(&abs, &opts, &prog);
            done.store(true, Relaxed);
            r
        })
    };

    if let Some(f) = &a.output_file {
        let attrs = json::Attrs(a.output_attrs.clone());
        let r = if f == "-" {
            let mut bw = BufWriter::with_capacity(65536, out);
            json::export(&root, &attrs, &mut bw).and_then(|()| bw.flush())
        } else {
            std::fs::File::create(f).and_then(|mut file| {
                let mut bw = BufWriter::with_capacity(65536, &mut file);
                json::export(&root, &attrs, &mut bw).and_then(|()| bw.flush())
            })
        };
        r.unwrap_or_else(|e| die(e));
        std::mem::forget(root); // exiting anyway: skip freeing ~1 allocation per item
        return;
    }

    if batch {
        let cfg = stdout::Cfg {
            fmt,
            apparent: a.show_apparent_size,
            show_count: a.show_item_count,
            color,
            reverse: a.reverse_sort,
            top: a.top,
            depth: a.depth,
            summarize: a.summarize,
        };
        let name = if a.input_file.is_some() {
            root.name.to_string()
        } else {
            scan_path
        };
        let mut bw = BufWriter::with_capacity(65536, out);
        stdout::print(&mut root, &name, &cfg, &mut bw)
            .and_then(|()| bw.flush())
            .unwrap_or_else(|e| die(e));
        std::mem::forget(root); // exiting anyway: skip freeing ~1 allocation per item
    } else {
        tui::run(tui::Source::Ready(root), tui_cfg()).unwrap_or_else(|e| die(e));
    }
}
