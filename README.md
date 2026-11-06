# rsdu

A Rust port of [ncdu](https://dev.yorhel.nl/ncdu): a fast disk usage analyzer for the
terminal with parallel scanning, an interactive TUI, plain-text output, and JSON
export/import in ncdu's export format. See [References](#references).

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/awaiden/rsdu/main/install.sh | sh
```

This downloads the latest release binary into `~/.local/bin` (override with `INSTALL_DIR`,
pin a version with `RSDU_VERSION=v0.1.0`). Read the script first if you prefer:
[install.sh](install.sh).

From source (Rust 1.85+):

```sh
cargo install --git https://github.com/awaiden/rsdu
```

## Usage

```sh
rsdu [OPTIONS] [PATH]     # interactive TUI (PATH defaults to the current directory)
rsdu -n ~                 # plain listing, no TUI
rsdu -n -s ~              # just the total
rsdu -n -t 10 /usr        # 10 largest files
rsdu -n --depth 2 .       # tree two levels deep
rsdu -d                   # mounted disks
rsdu -o out.json .        # export JSON (ncdu format)
rsdu -f out.json          # browse an export (read-only: deleting is disabled)
rsdu -I '.*\.log' /var    # skip paths matching a regex (files and directories)
```

`rsdu --help` lists every flag. Flags follow the names and short letters used by gdu where ncdu has no equivalent.

### TUI keys

| Key                                   | Action                                                 |
| ------------------------------------- | ------------------------------------------------------ |
| `j` `k` / `↑` `↓`                     | move                                                   |
| `l` `Enter` `→` / `h` `Backspace` `←` | open / parent directory                                |
| `a`                                   | toggle apparent size / disk usage                      |
| `c` `m`                               | show item count / mtime column                         |
| `s` `C` `n` `M`                       | sort by size / count / name / mtime (again to reverse) |
| `d`                                   | delete (asks for confirmation)                         |
| `?` / `q`                             | help / quit                                            |

Use `--no-delete` to disable deletion.

### JSON export

`rsdu -o` writes ncdu's export format, so the file loads in ncdu (`ncdu -f out.json`) and
rsdu reads ncdu's files (`ncdu -o out.json DIR`, then `rsdu -f out.json`). As in ncdu, a
directory entry holds only the directory's own size and readers add up the children. Files
written by gdu and by rsdu 0.1.0 (which wrote cumulative directory totals) are still imported
correctly. `-I` can be repeated for several patterns and applies to files as well as
directories; use `-X FILE` to read many patterns from a file.

## Benchmarks

Scan time, warm cache, mean of 7 interleaved runs, 12-core machine. All three tools build the
full tree and write a JSON export to `/dev/null` (`rsdu -n -p -o /dev/null`,
`gdu -n -p -o /dev/null`, `ncdu -0 -x -t 12 -o /dev/null`):

| Tree                          | rsdu   | gdu 5.32 | ncdu 2.9.1 (`-t 12`) |
| ----------------------------- | ------ | -------- | -------------------- |
| `/usr` (247k entries)         | 69 ms  | 262 ms   | 86 ms                |
| `~/Documents` (1.15M entries) | 372 ms | 1562 ms  | 478 ms               |

Peak memory on `~/Documents`: rsdu 198 MB, ncdu 78 MB, gdu 1.2 GB.

rsdu is about 4x to 4.2x faster than gdu and 20-22% faster than ncdu running on all cores
(ncdu's default is one thread, which took 1712 ms on `~/Documents` in an earlier run).
When only a total is needed (`rsdu -n -s`), rsdu does not keep the tree and finishes in
53 ms on `/usr` and 272 ms on `~/Documents` using about 75 MB.

Hardlinked files are counted once per directory subtree, so results are identical from run
to run regardless of thread timing.

Run it yourself with `python3 benchmark.py ~/some/dir` (needs only Python 3; `--help` lists
the options, and tools that are not installed are skipped).

Caveats: warm cache only (cold-cache and spinning-disk results can differ), one machine and
filesystem.

## References

rsdu is a port of ncdu and was written with the sources of both projects as reference:

- [ncdu](https://dev.yorhel.nl/ncdu) by Yoran Heling (Yorhel): the original disk usage
  analyzer and the JSON export format rsdu reads and writes. MIT license.
- [gdu](https://github.com/dundee/gdu) by Daniel Milde: a Go implementation of the same
  idea, whose parallel scanner, size accounting and command-line flags rsdu follows. MIT license.

## License

MIT, see [LICENSE](LICENSE). The copyright notices of ncdu and gdu are included there.
