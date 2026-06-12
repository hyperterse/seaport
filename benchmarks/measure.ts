#!/usr/bin/env bun
/**
 * Rich Seaport vs Harbor comparison across one or more datasets.
 *
 * Wraps every invocation in `/usr/bin/time -l` and records the factors that
 * distinguish a static Rust binary from a Python CLI:
 *
 *   - wall-clock time (mean / median / min / max / stddev)
 *   - CLI-process peak resident memory (RSS) and peak memory footprint
 *   - CPU time consumed (user + sys) and CPU utilisation
 *   - instructions retired and cycles elapsed (Apple Silicon perf counters)
 *   - clean-exit rate, install footprint, and cold first-run cost
 *
 * The heavy task work runs inside the shared Docker daemon for both tools, so
 * the `/usr/bin/time` rusage of each CLI process is a fair measure of harness
 * overhead -- the part each tool actually controls. A warm-up iteration is run
 * per target and discarded, so reported numbers are steady-state, not cold.
 *
 * Usage:
 *   bun benchmarks/measure.ts -p benchmarks/tasks/basic-oracle@8 \
 *       -d factory-ai/legacy-bench@3 -d terminal-bench/terminal-bench-2@1@0
 *
 * Target syntax: `<spec>[@iterations[@warmup]]`. `-p` is a local task/dataset
 * path, `-d` is a registered dataset name. `-i`/`--iterations` and `--warmup`
 * set the defaults for targets that omit them.
 */

import { spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { basename, dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const TIME_BIN = "/usr/bin/time";

// The /usr/bin/time -l report block always ends the process's stderr. Its first
// line carries real/user/sys; the rest are "<number>  <label>" rusage lines.
const HEADER_RE = /([\d.]+)\s+real\s+([\d.]+)\s+user\s+([\d.]+)\s+sys/;
const LABEL_RE = /^\s*(\d+)\s+(.+?)\s*$/;

interface Target {
  kind: "dataset" | "path";
  spec: string;
  label: string;
  iterations: number;
  warmup: number;
}

interface Run {
  iteration: number;
  returncode: number;
  wallSec: number;
  userSec: number | null;
  sysSec: number | null;
  maxRssBytes: number | null;
  peakFootprintBytes: number | null;
  instructions: number | null;
  cycles: number | null;
}

interface ToolResult {
  name: string;
  command: string[];
  cold: Run | null;
  runs: Run[];
  failures: string[];
  binaryBytes: number | null;
  installBytes: number | null;
}

const cleanExit = (run: Run): boolean => run.returncode === 0;
const cpuSec = (run: Run): number | null =>
  run.userSec === null || run.sysSec === null ? null : run.userSec + run.sysSec;

function parseTimeBlock(stderr: string): Record<string, number> {
  const lines = stderr.split("\n");
  let headerIdx = -1;
  for (let i = lines.length - 1; i >= 0; i--) {
    if (HEADER_RE.test(lines[i])) {
      headerIdx = i;
      break;
    }
  }
  if (headerIdx === -1) return {};

  const header = HEADER_RE.exec(lines[headerIdx])!;
  const block: Record<string, number> = {
    real: parseFloat(header[1]),
    user: parseFloat(header[2]),
    sys: parseFloat(header[3]),
  };
  for (const line of lines.slice(headerIdx + 1)) {
    const m = LABEL_RE.exec(line);
    if (m) block[m[2]] = parseInt(m[1], 10);
  }
  return block;
}

function runOnce(iteration: number, command: string[], cwd: string): { run: Run; detail: string } {
  const wrapped = [TIME_BIN, "-l", ...command];
  const started = performance.now();
  const completed = spawnSync(wrapped[0], wrapped.slice(1), {
    cwd,
    encoding: "utf8",
    maxBuffer: 256 * 1024 * 1024,
  });
  const wall = (performance.now() - started) / 1000;
  const block = parseTimeBlock(completed.stderr ?? "");
  const code = completed.status ?? 1;
  const run: Run = {
    iteration,
    returncode: code,
    wallSec: block.real ?? wall,
    userSec: block.user ?? null,
    sysSec: block.sys ?? null,
    maxRssBytes: block["maximum resident set size"] ?? null,
    peakFootprintBytes: block["peak memory footprint"] ?? null,
    instructions: block["instructions retired"] ?? null,
    cycles: block["cycles elapsed"] ?? null,
  };
  let detail = "";
  if (!cleanExit(run)) {
    detail = [
      `iteration ${iteration} exited ${code}`,
      (completed.stderr ?? "").trim().split("\n").filter((l) => !LABEL_RE.test(l) && !HEADER_RE.test(l)).slice(-3).join(" | "),
    ].join(" ");
  }
  return { run, detail };
}

function measureTool(
  name: string,
  command: string[],
  cwd: string,
  target: Target,
): ToolResult {
  const result: ToolResult = {
    name,
    command,
    cold: null,
    runs: [],
    failures: [],
    binaryBytes: null,
    installBytes: null,
  };

  for (let w = 1; w <= target.warmup; w++) {
    console.log(`  [${target.label}] ${name} warm-up ${w}/${target.warmup} (discarded)...`);
    const cold = runOnce(0, command, cwd);
    if (w === 1) result.cold = cold.run;
    console.log(`    cold: ${cold.run.wallSec.toFixed(2)}s rc=${cold.run.returncode}`);
  }

  for (let i = 1; i <= target.iterations; i++) {
    const { run, detail } = runOnce(i, command, cwd);
    result.runs.push(run);
    const rss = run.maxRssBytes === null ? "n/a" : `${Math.round(run.maxRssBytes / 1e6)}MB`;
    const cpu = cpuSec(run) === null ? "n/a" : `${cpuSec(run)!.toFixed(2)}s`;
    console.log(
      `  [${target.label}] ${name} ${i}/${target.iterations}: ${run.wallSec.toFixed(2)}s rss=${rss} cpu=${cpu} rc=${run.returncode}`,
    );
    if (detail) result.failures.push(detail);
  }
  return result;
}

function dirSizeBytes(path: string): number | null {
  const out = spawnSync("du", ["-sk", path], { encoding: "utf8" });
  if (out.status !== 0 || !out.stdout) return null;
  const kb = parseInt(out.stdout.trim().split(/\s+/)[0], 10);
  return Number.isNaN(kb) ? null : kb * 1024;
}

function harborInstallRoot(harborBin: string): string | null {
  let firstLine: string;
  try {
    firstLine = readFileSync(harborBin, "utf8").split("\n")[0];
  } catch {
    return null;
  }
  if (!firstLine.startsWith("#!")) return null;
  let dir = dirname(firstLine.slice(2).trim());
  while (dir !== "/" && dir !== ".") {
    if (basename(dir) === "harbor") return dir;
    dir = dirname(dir);
  }
  return null;
}

interface Agg {
  mean: number | null;
  median: number | null;
  min: number | null;
  max: number | null;
  stddev: number | null;
}

function mean(values: number[]): number | null {
  return values.length ? values.reduce((a, b) => a + b, 0) / values.length : null;
}

function agg(values: number[]): Agg {
  if (!values.length) return { mean: null, median: null, min: null, max: null, stddev: null };
  const sorted = [...values].sort((a, b) => a - b);
  const m = mean(values)!;
  const mid = Math.floor(sorted.length / 2);
  const median = sorted.length % 2 ? sorted[mid] : (sorted[mid - 1] + sorted[mid]) / 2;
  const stddev =
    values.length > 1
      ? Math.sqrt(values.reduce((a, v) => a + (v - m) ** 2, 0) / (values.length - 1))
      : 0;
  return { mean: m, median, min: sorted[0], max: sorted[sorted.length - 1], stddev };
}

// Timing aggregates over EVERY measured run (a dataset with a failing task is
// still a valid timing sample); clean-exit rate is tracked separately.
function summarize(result: ToolResult, attempts: number) {
  const runs = result.runs;
  const num = (pick: (r: Run) => number | null) =>
    runs.map(pick).filter((v): v is number => v !== null);

  return {
    name: result.name,
    command: result.command,
    attempts,
    clean_exits: runs.filter(cleanExit).length,
    cold_wall_sec: result.cold ? result.cold.wallSec : null,
    wall_sec: agg(runs.map((r) => r.wallSec)),
    mean_max_rss_bytes: mean(num((r) => r.maxRssBytes)),
    mean_peak_footprint_bytes: mean(num((r) => r.peakFootprintBytes)),
    mean_cpu_sec: mean(num(cpuSec)),
    mean_cpu_pct: mean(
      runs.filter((r) => cpuSec(r) !== null && r.wallSec > 0).map((r) => (100 * cpuSec(r)!) / r.wallSec),
    ),
    mean_instructions: mean(num((r) => r.instructions)),
    binary_bytes: result.binaryBytes,
    install_bytes: result.installBytes,
    failures: result.failures.slice(0, 4),
  };
}

type Summary = ReturnType<typeof summarize>;

const ratio = (h: number | null, s: number | null): number | null => (h && s ? h / s : null);
const mb = (v: number | null): string => (v === null ? "n/a" : `${Math.round(v / 1e6)} MB`);
const secs = (v: number | null): string => (v === null ? "n/a" : `${v.toFixed(2)}s`);
const fx = (v: number | null): string => (v === null ? "n/a" : `${v.toFixed(1)}×`);
const bn = (v: number | null): string => (v === null ? "n/a" : `${(v / 1e9).toFixed(2)} B`);

interface DatasetReport {
  label: string;
  kind: string;
  spec: string;
  iterations: number;
  tools: { seaport: Summary; harbor: Summary };
  comparison: Record<string, number | null>;
}

function markdown(report: any): string {
  const lines: string[] = [
    "# Seaport vs Harbor: Rich Benchmark",
    "",
    `Date: ${report.date}`,
    `Datasets: ${report.datasets.map((d: DatasetReport) => `\`${d.label}\``).join(", ")}`,
    "",
    "Environment:",
    "",
    `- OS: ${report.env.os}`,
    `- Harbor: ${report.env.harbor_version}`,
    "- Seaport: local release build",
    "- Harness: `benchmarks/measure.ts` (Bun)",
    "",
    "## Summary across datasets",
    "",
    "| Dataset | Iters | Seaport wall | Harbor wall | Speedup | Seaport RSS | Harbor RSS | Lighter | Clean exits (S / H) |",
    "| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |",
  ];
  for (const d of report.datasets as DatasetReport[]) {
    const s = d.tools.seaport;
    const h = d.tools.harbor;
    const c = d.comparison;
    lines.push(
      `| \`${d.label}\` | ${d.iterations} | ${secs(s.wall_sec.mean)} | ${secs(h.wall_sec.mean)} | ${fx(c.wall_speedup)} | ${mb(s.mean_max_rss_bytes)} | ${mb(h.mean_max_rss_bytes)} | ${fx(c.rss_ratio)} | ${s.clean_exits}/${s.attempts} · ${h.clean_exits}/${h.attempts} |`,
    );
  }

  const g = report.global;
  lines.push(
    "",
    "Install footprint (static, one-shot): Seaport " +
      `${mb(g.seaport_install_bytes)} vs Harbor ${mb(g.harbor_install_bytes)} ` +
      `(${fx(g.footprint_ratio)} smaller).`,
    "",
  );

  for (const d of report.datasets as DatasetReport[]) {
    const s = d.tools.seaport;
    const h = d.tools.harbor;
    const c = d.comparison;
    lines.push(
      `## ${d.label}`,
      "",
      `Target: \`${d.kind} ${d.spec}\` · ${d.iterations} measured iteration(s).`,
      "",
      "| Factor | Seaport | Harbor | Seaport advantage |",
      "| --- | ---: | ---: | ---: |",
      `| Wall-clock (mean) | ${secs(s.wall_sec.mean)} | ${secs(h.wall_sec.mean)} | ${fx(c.wall_speedup)} faster |`,
      `| Peak memory (RSS) | ${mb(s.mean_max_rss_bytes)} | ${mb(h.mean_max_rss_bytes)} | ${fx(c.rss_ratio)} lighter |`,
      `| CPU time consumed | ${secs(s.mean_cpu_sec)} | ${secs(h.mean_cpu_sec)} | ${fx(c.cpu_ratio)} less |`,
      `| Instructions retired | ${bn(s.mean_instructions)} | ${bn(h.mean_instructions)} | ${fx(c.instr_ratio)} fewer |`,
      `| Cold first run | ${secs(s.cold_wall_sec)} | ${secs(h.cold_wall_sec)} | ${fx(c.cold_ratio)} faster |`,
      `| Clean exits | ${s.clean_exits}/${s.attempts} | ${h.clean_exits}/${h.attempts} | — |`,
      "",
      `Wall-clock spread — Seaport: mean ${secs(s.wall_sec.mean)}, median ${secs(s.wall_sec.median)}, min ${secs(s.wall_sec.min)}, max ${secs(s.wall_sec.max)}, σ ${secs(s.wall_sec.stddev)}. ` +
        `Harbor: mean ${secs(h.wall_sec.mean)}, median ${secs(h.wall_sec.median)}, min ${secs(h.wall_sec.min)}, max ${secs(h.wall_sec.max)}, σ ${secs(h.wall_sec.stddev)}.`,
      "",
    );
  }

  lines.push(
    "## Method",
    "",
    "Each invocation is wrapped in `/usr/bin/time -l`; the reported memory, CPU, and",
    "perf-counter figures are the CLI process's own rusage. The heavy container work",
    "runs in the shared Docker daemon for both tools, so what is left is harness",
    "overhead. Warm-up runs are discarded so steady-state numbers are not skewed by",
    "the first cold image build. Timing aggregates over every measured run; a dataset",
    "with a failing task is still a valid timing sample, and clean-exit counts are",
    "tracked separately.",
    "",
  );
  return lines.join("\n").replace(/\s+$/, "") + "\n";
}

function parseTargets(argv: string[]): { targets: Target[]; output?: string; seaportBin?: string; harborBin: string } {
  const targets: Target[] = [];
  let defaultIters = 8;
  let defaultWarmup = 1;
  let output: string | undefined;
  let seaportBin: string | undefined;
  let harborBin = "harbor";
  const pending: { kind: "dataset" | "path"; raw: string }[] = [];

  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === "-d" || a === "--dataset") pending.push({ kind: "dataset", raw: argv[++i] });
    else if (a === "-p" || a === "--path") pending.push({ kind: "path", raw: argv[++i] });
    else if (a === "-i" || a === "--iterations") defaultIters = parseInt(argv[++i], 10);
    else if (a === "--warmup") defaultWarmup = parseInt(argv[++i], 10);
    else if (a === "--seaport-bin") seaportBin = argv[++i];
    else if (a === "--harbor-bin") harborBin = argv[++i];
    else if (a === "--output") output = argv[++i];
  }

  for (const p of pending) {
    const [spec, itersRaw, warmupRaw] = p.raw.split("@");
    targets.push({
      kind: p.kind,
      spec,
      label: p.kind === "path" ? basename(spec) : spec,
      iterations: itersRaw ? parseInt(itersRaw, 10) : defaultIters,
      warmup: warmupRaw !== undefined ? parseInt(warmupRaw, 10) : defaultWarmup,
    });
  }
  return { targets, output, seaportBin, harborBin };
}

function captureLine(command: string, cmdArgs: string[]): string {
  const out = spawnSync(command, cmdArgs, { encoding: "utf8" });
  return (out.stdout ?? "").trim();
}

function main(): number {
  const { targets, output, seaportBin: sbArg, harborBin: hbArg } = parseTargets(process.argv.slice(2));
  if (!targets.length) {
    console.error("no targets; pass at least one -p <path> or -d <dataset>");
    return 2;
  }
  if (!statSync(TIME_BIN, { throwIfNoEntry: false })) {
    console.error(`${TIME_BIN} is required (macOS/BSD time)`);
    return 2;
  }
  const seaportBin = resolve(sbArg ?? join(ROOT, "target", "release", "seaport"));
  if (!statSync(seaportBin, { throwIfNoEntry: false })) {
    console.error(`seaport binary not found: ${seaportBin}`);
    return 2;
  }
  const harborBin = Bun.which(hbArg);
  if (!harborBin) {
    console.error(`harbor not found on PATH: ${hbArg}`);
    return 2;
  }

  const binaryBytes = statSync(seaportBin).size;
  const installRoot = harborInstallRoot(harborBin);
  const harborInstallBytes = installRoot ? dirSizeBytes(installRoot) : null;

  const datasets: DatasetReport[] = [];
  for (const target of targets) {
    const flag = target.kind === "dataset" ? "-d" : "-p";
    const spec = target.kind === "path" ? resolve(target.spec) : target.spec;
    console.log(`\n=== ${target.label} (${target.iterations} iters, ${target.warmup} warm-up) ===`);

    const seaTmp = mkdtempSync(join(tmpdir(), "seaport-measure-"));
    let sea: Summary;
    try {
      const cmd = [seaportBin, "run", flag, spec, "-a", "oracle", "--backend", "docker", "--jobs-dir", join(seaTmp, "jobs")];
      const r = measureTool("seaport", cmd, ROOT, target);
      r.binaryBytes = binaryBytes;
      r.installBytes = binaryBytes;
      sea = summarize(r, target.iterations);
    } finally {
      rmSync(seaTmp, { recursive: true, force: true });
    }

    const harTmp = mkdtempSync(join(tmpdir(), "harbor-measure-"));
    let har: Summary;
    try {
      const cmd = [harborBin, "run", flag, spec, "-a", "oracle"];
      const r = measureTool("harbor", cmd, harTmp, target);
      r.installBytes = harborInstallBytes;
      har = summarize(r, target.iterations);
    } finally {
      rmSync(harTmp, { recursive: true, force: true });
    }

    datasets.push({
      label: target.label,
      kind: target.kind,
      spec,
      iterations: target.iterations,
      tools: { seaport: sea, harbor: har },
      comparison: {
        wall_speedup: ratio(har.wall_sec.mean, sea.wall_sec.mean),
        rss_ratio: ratio(har.mean_max_rss_bytes, sea.mean_max_rss_bytes),
        cpu_ratio: ratio(har.mean_cpu_sec, sea.mean_cpu_sec),
        instr_ratio: ratio(har.mean_instructions, sea.mean_instructions),
        cold_ratio: ratio(har.cold_wall_sec, sea.cold_wall_sec),
      },
    });
  }

  const report = {
    date: captureLine("date", ["+%Y-%m-%d"]),
    env: {
      os: captureLine("uname", ["-sm"]),
      harbor_version: captureLine(harborBin, ["--version"]) || "unknown",
    },
    global: {
      seaport_install_bytes: binaryBytes,
      harbor_install_bytes: harborInstallBytes,
      footprint_ratio: ratio(harborInstallBytes, binaryBytes),
    },
    datasets,
  };

  const out = resolve(output ?? join(ROOT, "benchmarks", "results", `${report.date}-datasets.json`));
  const mdPath = out.replace(/\.json$/, ".md");
  writeFileSync(out, JSON.stringify(report, null, 2) + "\n");
  writeFileSync(mdPath, markdown(report));

  console.log();
  console.log(markdown(report));
  console.log(`Wrote ${out}`);
  console.log(`Wrote ${mdPath}`);
  return 0;
}

process.exit(main());
