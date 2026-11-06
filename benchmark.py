#!/usr/bin/env python3
"""Compare scan speed and peak memory of rsdu, gdu and ncdu on your own directories.

    python3 benchmark.py ~/Documents /usr
    python3 benchmark.py -n 10 --mode total .

Tools that are not installed are skipped. rsdu is looked up in ./target/release/rsdu
(cargo build --release) and then on PATH. Runs are interleaved so no tool gets a cache
advantage, every tool stays on one filesystem (-x) so they all scan the same data, and one
untimed warm-up run per tool fills the page cache first: the numbers
are warm-cache numbers, so cold-cache and spinning-disk results can differ.

Modes:
  export  every tool builds the full tree and writes a JSON export to /dev/null (default,
          the fairest comparison)
  total   rsdu and gdu print only the total (-s); ncdu has no such mode and still exports
"""
import argparse
import os
import shutil
import statistics
import subprocess
import sys
import time


def find_rsdu(explicit):
    local = os.path.join(os.path.dirname(os.path.abspath(__file__)), "target", "release", "rsdu")
    return explicit or (local if os.access(local, os.X_OK) else shutil.which("rsdu"))


def commands(args, path):
    rsdu, gdu, ncdu = args.rsdu_bin, args.gdu_bin, args.ncdu_bin
    out = "-s" if args.mode == "total" else "-o"
    tail = [] if args.mode == "total" else ["/dev/null"]
    cmds = {}
    if rsdu:
        cmds["rsdu"] = [rsdu, "-n", "-p", "-x", out, *tail, path]
    if gdu:
        cmds["gdu"] = [gdu, "-n", "-p", "-x", out, *tail, path]
    if ncdu:
        cmds["ncdu"] = [ncdu, "-0", "-x", "-t", str(args.threads), "-o", "/dev/null", path]
    return cmds


def run(cmd):
    """Run once; return (seconds, peak RSS in MB, exit code)."""
    start = time.perf_counter()
    proc = subprocess.Popen(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    _, status, usage = os.wait4(proc.pid, 0)
    elapsed = time.perf_counter() - start
    rss = usage.ru_maxrss / (1024 * 1024 if sys.platform == "darwin" else 1024)  # macOS reports bytes
    return elapsed, rss, os.waitstatus_to_exitcode(status)


def bench(args, path):
    cmds = commands(args, path)
    for name in list(cmds):  # warm-up, also drops tools that fail (e.g. old ncdu without -t)
        if run(cmds[name])[2] != 0:
            print(f"  skipping {name}: command failed: {' '.join(cmds[name])}", file=sys.stderr)
            del cmds[name]
    times = {k: [] for k in cmds}
    mem = {k: 0.0 for k in cmds}
    for _ in range(args.runs):
        for name, cmd in cmds.items():
            t, rss, _ = run(cmd)
            times[name].append(t)
            mem[name] = max(mem[name], rss)
    return times, mem


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("paths", nargs="*", default=["."], help="directories to scan (default: current directory)")
    p.add_argument("-n", "--runs", type=int, default=5, help="timed runs per tool (default 5)")
    p.add_argument("--mode", choices=["export", "total"], default="export")
    p.add_argument("--threads", type=int, default=os.cpu_count() or 1, help="threads for ncdu -t (default: all cores)")
    p.add_argument("--rsdu", dest="rsdu_bin", help="path to the rsdu binary")
    p.add_argument("--gdu", dest="gdu_bin", default=shutil.which("gdu"), help="path to gdu")
    p.add_argument("--ncdu", dest="ncdu_bin", default=shutil.which("ncdu"), help="path to ncdu (2.7+ for -t)")
    args = p.parse_args()
    args.rsdu_bin = find_rsdu(args.rsdu_bin)
    for name in ("rsdu_bin", "gdu_bin", "ncdu_bin"):
        exe = getattr(args, name)
        if exe and not (shutil.which(exe) or os.access(exe, os.X_OK)):
            sys.exit(f"{exe}: not an executable file")

    found = {k: v for k, v in (("rsdu", args.rsdu_bin), ("gdu", args.gdu_bin), ("ncdu", args.ncdu_bin)) if v}
    if not found:
        sys.exit("No tools found: build rsdu (cargo build --release) or install gdu/ncdu.")
    print("tools:", ", ".join(f"{k}={v}" for k, v in found.items()))
    print(f"mode: {args.mode}, runs: {args.runs}, cores: {os.cpu_count()}, ncdu threads: {args.threads}\n")

    for path in args.paths:
        path = os.path.abspath(path)
        print(f"### {path}")
        times, mem = bench(args, path)
        print("\n| tool | mean | min | max | peak memory |\n|---|---|---|---|---|")
        for name, ts in times.items():
            print(f"| {name} | {statistics.mean(ts) * 1000:.0f} ms | {min(ts) * 1000:.0f} ms | "
                  f"{max(ts) * 1000:.0f} ms | {mem[name]:.0f} MB |")
        print()


if __name__ == "__main__":
    main()
