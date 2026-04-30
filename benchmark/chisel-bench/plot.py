#!/usr/bin/env python3
"""Generate benchmark graphs from chisel-style results.json.

Schema (new):
  {
    "timestamp": "...",
    "config": {"runs": N, "warmups": M},
    "results": [
      {"tool": "...", "bytes": N, "samples_ms": [...], "median_ms": x, "min_ms": y, "max_ms": z},
      ...
    ]
  }
"""

import json
import sys
from pathlib import Path

import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt
import numpy as np

DIRECT_COLOR = "#6B7280"
RUSNEL_COLOR = "#2563EB"
CHISEL_COLOR = "#9333EA"
BG_COLOR = "#FAFAFA"
GRID_COLOR = "#E5E7EB"


def fmt_bytes(b):
    if b >= 1_000_000:
        return f"{b // 1_000_000}MB"
    if b >= 1_000:
        return f"{b // 1_000}KB"
    return f"{b}B"


def load(path):
    with open(path) as f:
        data = json.load(f)
    if isinstance(data, list):
        # Backwards compat with the old flat schema (tool/bytes/ms).
        return {
            "config": {"runs": 1, "warmups": 0},
            "results": [
                {"tool": r["tool"], "bytes": r["bytes"],
                 "samples_ms": [r["ms"]], "median_ms": r["ms"],
                 "min_ms": r["ms"], "max_ms": r["ms"]}
                for r in data
            ],
        }
    return data


def by_tool_size(results):
    out = {}
    for r in results:
        out.setdefault(r["tool"], {})[r["bytes"]] = r
    return out


def make_chart(data, out_dir):
    results = data["results"]
    cfg = data.get("config", {})
    table = by_tool_size(results)

    sizes = sorted({r["bytes"] for r in results})
    labels = [fmt_bytes(s) for s in sizes]

    def col(tool, key):
        return [table[tool][s][key] for s in sizes]

    direct_med = col("direct", "median_ms")
    rusnel_med = col("rusnel", "median_ms")
    chisel_med = col("chisel", "median_ms")

    # Asymmetric error bars from min/max around median.
    def err(tool):
        med = np.array(col(tool, "median_ms"))
        lo = np.array(col(tool, "min_ms"))
        hi = np.array(col(tool, "max_ms"))
        return np.array([med - lo, hi - med])

    fig, ax = plt.subplots(figsize=(10, 5))
    fig.patch.set_facecolor(BG_COLOR)
    ax.set_facecolor(BG_COLOR)

    x = np.arange(len(sizes))
    width = 0.25

    ax.bar(x - width, direct_med, width, yerr=err("direct"), label="Direct",
           color=DIRECT_COLOR, edgecolor="white", linewidth=1, capsize=3,
           error_kw={"elinewidth": 0.8, "ecolor": "#374151"})
    ax.bar(x, rusnel_med, width, yerr=err("rusnel"), label="Rusnel (QUIC)",
           color=RUSNEL_COLOR, edgecolor="white", linewidth=1, capsize=3,
           error_kw={"elinewidth": 0.8, "ecolor": "#1E40AF"})
    ax.bar(x + width, chisel_med, width, yerr=err("chisel"), label="Chisel (SSH/WS)",
           color=CHISEL_COLOR, edgecolor="white", linewidth=1, capsize=3,
           error_kw={"elinewidth": 0.8, "ecolor": "#6B21A8"})

    ax.set_ylabel("Time (ms, median)", fontsize=11)
    ax.set_xlabel("Payload Size", fontsize=11)
    runs = cfg.get("runs", "?")
    warmups = cfg.get("warmups", "?")
    ax.set_title(
        f"HTTP Request Through Tunnel — Median of {runs} runs (warmup={warmups}, error bars = min/max)",
        fontsize=12, fontweight="bold", pad=15,
    )
    ax.set_xticks(x)
    ax.set_xticklabels(labels, fontsize=10)
    ax.set_yscale("log")
    ax.legend(fontsize=10)
    ax.grid(axis="y", color=GRID_COLOR, linewidth=0.8, alpha=0.7)
    ax.set_axisbelow(True)
    ax.spines[["top", "right"]].set_visible(False)

    fig.tight_layout()
    fig.savefig(out_dir / "chisel-bench.png", dpi=150, bbox_inches="tight")
    plt.close(fig)

    # Overhead chart — median(rusnel)/median(direct) and median(chisel)/median(direct).
    # Restrict to sizes >= 1KB where the measurement isn't dominated by clock noise.
    overhead_mask = [s >= 1000 for s in sizes]
    o_sizes = [s for s, m in zip(sizes, overhead_mask) if m]
    o_labels = [fmt_bytes(s) for s in o_sizes]
    o_x = np.arange(len(o_sizes))

    o_direct = [d for d, m in zip(direct_med, overhead_mask) if m]
    o_rusnel = [r for r, m in zip(rusnel_med, overhead_mask) if m]
    o_chisel = [c for c, m in zip(chisel_med, overhead_mask) if m]

    rusnel_overhead = [r / d for r, d in zip(o_rusnel, o_direct)]
    chisel_overhead = [c / d for c, d in zip(o_chisel, o_direct)]

    fig2, ax2 = plt.subplots(figsize=(10, 5))
    fig2.patch.set_facecolor(BG_COLOR)
    ax2.set_facecolor(BG_COLOR)

    ax2.bar(o_x - 0.15, rusnel_overhead, 0.3, label="Rusnel (QUIC)",
            color=RUSNEL_COLOR, edgecolor="white", linewidth=1)
    ax2.bar(o_x + 0.15, chisel_overhead, 0.3, label="Chisel (SSH/WS)",
            color=CHISEL_COLOR, edgecolor="white", linewidth=1)

    ymax = max(max(rusnel_overhead), max(chisel_overhead))
    for i, (rv, cv) in enumerate(zip(rusnel_overhead, chisel_overhead)):
        ax2.text(i - 0.15, rv + ymax * 0.02, f"{rv:.1f}x",
                 ha="center", fontsize=9, fontweight="bold", color=RUSNEL_COLOR)
        ax2.text(i + 0.15, cv + ymax * 0.02, f"{cv:.1f}x",
                 ha="center", fontsize=9, fontweight="bold", color=CHISEL_COLOR)

    ax2.axhline(y=1.0, color=DIRECT_COLOR, linestyle="--",
                linewidth=1, alpha=0.5, label="Direct (1.0x)")
    ax2.set_ylabel("Overhead vs Direct (median)", fontsize=11)
    ax2.set_xlabel("Payload Size", fontsize=11)
    ax2.set_title("Tunnel Overhead — Lower is Better", fontsize=13, fontweight="bold", pad=15)
    ax2.set_xticks(o_x)
    ax2.set_xticklabels(o_labels, fontsize=10)
    ax2.legend(fontsize=10)
    ax2.grid(axis="y", color=GRID_COLOR, linewidth=0.8, alpha=0.7)
    ax2.set_axisbelow(True)
    ax2.spines[["top", "right"]].set_visible(False)

    fig2.tight_layout()
    fig2.savefig(out_dir / "overhead.png", dpi=150, bbox_inches="tight")
    plt.close(fig2)


def main():
    if len(sys.argv) < 3:
        print(f"Usage: {sys.argv[0]} <results.json> <output_dir>")
        sys.exit(1)

    results_file = Path(sys.argv[1])
    out_dir = Path(sys.argv[2])
    out_dir.mkdir(parents=True, exist_ok=True)

    data = load(results_file)
    make_chart(data, out_dir)
    print(f"  chisel-bench: {out_dir}/chisel-bench.png")
    print(f"  overhead:     {out_dir}/overhead.png")


if __name__ == "__main__":
    main()
