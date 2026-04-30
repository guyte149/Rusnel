#!/usr/bin/env python3
"""Generate benchmark comparison graphs from results.json."""

import json
import sys
from pathlib import Path

import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt
import matplotlib.ticker as ticker

RUSNEL_COLOR = "#2563EB"
CHISEL_COLOR = "#9333EA"
BG_COLOR = "#FAFAFA"
GRID_COLOR = "#E5E7EB"


def make_throughput_chart(data, out_dir):
    r = data["throughput_mbps"]["rusnel"]
    c = data["throughput_mbps"]["chisel"]
    diff = ((r - c) / c * 100) if c > 0 else 0

    fig, ax = plt.subplots(figsize=(7, 4))
    fig.patch.set_facecolor(BG_COLOR)
    ax.set_facecolor(BG_COLOR)

    bars = ax.barh(
        ["Chisel\n(SSH/WebSocket)", "Rusnel\n(QUIC)"],
        [c, r],
        color=[CHISEL_COLOR, RUSNEL_COLOR],
        height=0.5,
        edgecolor="white",
        linewidth=1.5,
    )

    for bar, val in zip(bars, [c, r]):
        ax.text(
            bar.get_width() + max(r, c) * 0.02,
            bar.get_y() + bar.get_height() / 2,
            f"{val:.0f} MB/s",
            va="center",
            fontsize=12,
            fontweight="bold",
        )

    ax.set_xlabel("Throughput (MB/s)", fontsize=11)
    ax.set_title(
        f"TCP Tunnel Throughput — Rusnel is {diff:+.0f}% faster",
        fontsize=13,
        fontweight="bold",
        pad=15,
    )
    ax.set_xlim(0, max(r, c) * 1.25)
    ax.xaxis.set_major_formatter(ticker.FuncFormatter(lambda x, _: f"{x:.0f}"))
    ax.grid(axis="x", color=GRID_COLOR, linewidth=0.8)
    ax.set_axisbelow(True)
    ax.spines[["top", "right"]].set_visible(False)

    cfg = data["config"]
    warmup = cfg.get("throughput_warmup", 0)
    ax.text(
        0.99, -0.15,
        f"iperf3, {cfg['payload_mb']}MB × {cfg['throughput_runs']} runs"
        + (f" (+{warmup} warmup)" if warmup else "")
        + ", median",
        transform=ax.transAxes, ha="right", fontsize=8, color="#6B7280",
    )

    fig.tight_layout()
    fig.savefig(out_dir / "throughput.png", dpi=150, bbox_inches="tight")
    fig.savefig(out_dir / "throughput.svg", bbox_inches="tight")
    plt.close(fig)


def make_latency_chart(data, out_dir):
    rl = data["latency_ms"]["rusnel"]
    cl = data["latency_ms"]["chisel"]

    labels = ["avg", "p50", "p99"]
    rusnel_vals = [rl["avg"], rl["p50"], rl["p99"]]
    chisel_vals = [cl["avg"], cl["p50"], cl["p99"]]

    x = range(len(labels))
    width = 0.32

    fig, ax = plt.subplots(figsize=(7, 4))
    fig.patch.set_facecolor(BG_COLOR)
    ax.set_facecolor(BG_COLOR)

    bars_r = ax.bar(
        [i - width / 2 for i in x], rusnel_vals, width,
        label="Rusnel (QUIC)", color=RUSNEL_COLOR, edgecolor="white", linewidth=1.2,
    )
    bars_c = ax.bar(
        [i + width / 2 for i in x], chisel_vals, width,
        label="Chisel (SSH/WebSocket)", color=CHISEL_COLOR, edgecolor="white", linewidth=1.2,
    )

    for bars in [bars_r, bars_c]:
        for bar in bars:
            h = bar.get_height()
            ax.text(
                bar.get_x() + bar.get_width() / 2, h + max(rl["p99"], cl["p99"]) * 0.03,
                f"{h:.2f}", ha="center", va="bottom", fontsize=9, fontweight="bold",
            )

    ax.set_ylabel("Latency (ms)", fontsize=11)
    ax.set_title(
        "TCP Tunnel Latency — Lower is Better",
        fontsize=13, fontweight="bold", pad=15,
    )
    ax.set_xticks(list(x))
    ax.set_xticklabels(labels, fontsize=11)
    ax.legend(fontsize=10, framealpha=0.9)
    ax.grid(axis="y", color=GRID_COLOR, linewidth=0.8)
    ax.set_axisbelow(True)
    ax.spines[["top", "right"]].set_visible(False)
    ax.set_ylim(0, max(rl["p99"], cl["p99"]) * 1.3)

    cfg = data["config"]
    warmup = cfg.get("latency_warmup", 0)
    ax.text(
        0.99, -0.12,
        f"{cfg['latency_requests']} echo round-trips × {cfg['latency_payload_bytes']}B"
        + (f" (+{warmup} warmup)" if warmup else ""),
        transform=ax.transAxes, ha="right", fontsize=8, color="#6B7280",
    )

    fig.tight_layout()
    fig.savefig(out_dir / "latency.png", dpi=150, bbox_inches="tight")
    fig.savefig(out_dir / "latency.svg", bbox_inches="tight")
    plt.close(fig)


def main():
    if len(sys.argv) < 3:
        print(f"Usage: {sys.argv[0]} <results.json> <output_dir>")
        sys.exit(1)

    results_file = Path(sys.argv[1])
    out_dir = Path(sys.argv[2])
    out_dir.mkdir(parents=True, exist_ok=True)

    with open(results_file) as f:
        data = json.load(f)

    make_throughput_chart(data, out_dir)
    make_latency_chart(data, out_dir)

    print(f"  throughput: {out_dir}/throughput.png, {out_dir}/throughput.svg")
    print(f"  latency:    {out_dir}/latency.png, {out_dir}/latency.svg")


if __name__ == "__main__":
    main()
