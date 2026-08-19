/**
 * 回合报告导出——把一场 PK 渲染成自包含 HTML 单文件。
 *
 * 纯前端拼装:数据全部来自 store(设置、选手、计分) + 已抓取的 diff;
 * 文件树由调用方用 getFileTree 抓取后拍平成相对路径传进来。产物不引
 * 任何外部资源,浏览器直接打开即可分享(同事 / V2EX / 存档)。
 */

import { getAgentLabel } from "@/lib/custom-agents"
import type { PkContestant, PkRound } from "@/stores/pk-arena-store"

function esc(raw: string): string {
  return raw
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
}

function formatMs(ms: number): string {
  const seconds = Math.round(ms / 1000)
  if (seconds < 60) return `${seconds}s`
  return `${Math.floor(seconds / 60)}m${String(seconds % 60).padStart(2, "0")}s`
}

const PERMISSION_LABEL: Record<PkRound["permissionMode"], string> = {
  default: "ask every time",
  acceptEdits: "auto-accept edits",
  bypassPermissions: "full auto",
}

const EFFORT_LABEL: Record<PkRound["effort"], string> = {
  default: "default",
  low: "low",
  medium: "medium",
  high: "high",
  max: "max",
}

/** diff 文本 → 带红绿标注的 <pre> 行。 */
function renderDiff(diff: string | null): string {
  if (!diff) return "<p class=dim>（无 diff 数据）</p>"
  if (diff.trim() === "")
    return "<p class=dim>（无可比较内容:选手未改动工作区）</p>"
  const lines = diff.split("\n")
  const body = lines
    .map((line) => {
      const cls = line.startsWith("+")
        ? "add"
        : line.startsWith("-")
          ? "del"
          : line.startsWith("@@")
            ? "hunk"
            : ""
      const label = line[0] ?? " "
      return `<div class="d ${cls}"><span class="sig">${esc(label)}</span>${esc(
        line.slice(1)
      )}</div>`
    })
    .join("")
  return `<pre>${body}</pre>`
}

function renderFiles(files: string[]): string {
  if (files.length === 0) return "<p class=dim>（没有产出文件）</p>"
  return (
    "<ul class=files>" +
    files.map((f) => `<li>${esc(f)}</li>`).join("") +
    "</ul>"
  )
}

const STATUS_LABEL: Record<PkContestant["status"], string> = {
  preparing: "preparing",
  connecting: "connecting",
  ready: "ready",
  running: "running",
  done: "done",
  error: "failed",
  canceled: "canceled",
}

export function buildPkReportHtml(
  round: PkRound,
  filesByAgent: Record<string, string[]>
): string {
  const finished = round.contestants.every(
    (c) =>
      c.status === "done" || c.status === "error" || c.status === "canceled"
  )
  const durationMs =
    round.contestants.reduce<number | null>((acc, c) => {
      if (c.durationMs == null) return acc
      return Math.max(acc ?? 0, c.durationMs)
    }, null) ?? null

  const rows = round.contestants
    .map((c) => {
      const stats = diffStats(c.diff)
      return `
      <tr>
        <td class=agent>${esc(getAgentLabel(c.agentType))}</td>
        <td>${esc(STATUS_LABEL[c.status])}</td>
        <td>${c.durationMs != null ? formatMs(c.durationMs) : "—"}</td>
        <td>${c.usage ? c.usage.outputTokens : "—"}</td>
        <td>${c.usage ? c.usage.turnCount : "—"}</td>
        <td><span class=add>+${stats.added}</span> <span class=del>−${stats.removed}</span></td>
        <td>${(filesByAgent[c.agentType] ?? []).length}</td>
      </tr>`
    })
    .join("")

  const sections = round.contestants
    .map((c) => {
      const files = filesByAgent[c.agentType] ?? []
      const modelLine =
        c.selectedModel != null ? ` · 模型 ${esc(c.selectedModel)}` : ""
      const effortLine =
        c.selectedEffort != null ? ` · 思考 ${esc(c.selectedEffort)}` : ""
      return `
      <details class=agent-sec ${!finished || c.status === "running" ? "" : ""}>
        <summary>
          <strong>${esc(getAgentLabel(c.agentType))}</strong>
          <span class=dim>${modelLine}${effortLine} · ${formatMs(
            c.durationMs ?? 0
          )}</span>
        </summary>
        <h4>产出文件</h4>
        ${renderFiles(files)}
        <h4>Diff（对比基准分支）</h4>
        ${renderDiff(c.diff)}
      </details>`
    })
    .join("")

  const statusText = finished
    ? "已结束"
    : round.status === "ready"
      ? "就绪"
      : "进行中"

  const judgeSection =
    round.judgeResult != null && round.judgeResult.scores.length > 0
      ? (() => {
          const scoreRows = round.judgeResult.scores
            .slice()
            .sort((a, b) => a.rank - b.rank)
            .map(
              (s) => `
          <tr>
            <td class=agent>${esc(getAgentLabel(s.agentType))}</td>
            <td><b>#${s.rank}</b></td>
            <td>${s.score}</td>
            <td>${esc(s.comment)}</td>
          </tr>`
            )
            .join("")
          return `
  <h2>⚖ 裁判评分</h2>
  <p class=meta>${esc(round.judgeResult.summary)}</p>
  <table>
    <thead><tr><th>选手</th><th>排名</th><th>分数</th><th>点评</th></tr></thead>
    <tbody>${scoreRows}</tbody>
  </table>`
        })()
      : ""

  return `<!doctype html>
<html lang="zh-CN">
<head>
<meta charset="utf-8" />
<meta name="viewport" content="width=device-width, initial-scale=1" />
<title>PK 报告 · ${esc(round.task)}</title>
<style>
  :root { color-scheme: light dark; }
  body { font: 14px/1.6 -apple-system, "Segoe UI", Roboto, "PingFang SC", sans-serif;
         max-width: 900px; margin: 0 auto; padding: 24px; color: #1a1a1a; }
  @media (prefers-color-scheme: dark) { body { color: #e6e6e6; } }
  h1 { font-size: 20px; } h2 { font-size: 15px; margin-top: 32px;
       border-bottom: 1px solid #8883; padding-bottom: 6px; }
  .meta { color: #666; font-size: 13px; } @media (prefers-color-scheme: dark) { .meta { color: #aaa; } }
  .meta b { color: #333; } @media (prefers-color-scheme: dark) { .meta b { color: #ddd; } }
  table { border-collapse: collapse; width: 100%; margin: 8px 0; }
  th, td { border-bottom: 1px solid #8883; padding: 6px 10px; text-align: left;
           font-size: 13px; }
  th { font-weight: 600; }
  td.agent { font-weight: 600; }
  .add { color: #15803d; } @media (prefers-color-scheme: dark) { .add { color: #4ade80; } }
  .del { color: #b91c1c; } @media (prefers-color-scheme: dark) { .del { color: #f87171; } }
  .dim { color: #888; }
  details.agent-sec { border: 1px solid #8883; border-radius: 8px; margin: 8px 0; padding: 0 12px; }
  summary { padding: 10px 0; cursor: pointer; }
  h4 { margin: 12px 0 6px; font-size: 13px; }
  pre { overflow-x: auto; background: #0002; border-radius: 6px; padding: 8px;
        font: 12px/1.5 ui-monospace, SFMono-Regular, Menlo, monospace; }
  .d.add { background: #4ade8022; } .d.del { background: #f8717122; } .d.hunk { background: #00000022; }
  .sig { display: inline-block; width: 14px; color: #888; user-select: none; }
  ul.files { margin: 4px 0; } @media (prefers-color-scheme: dark) { pre { background: #ffffff0d; } }
</style>
</head>
<body>
  <h1>⚔ PK 报告 — ${esc(round.task)}</h1>
  <p class=meta>
    <b>状态</b> ${statusText} · <b>时间</b> ${new Date(round.createdAt).toLocaleString()} ·
    <b>选手</b> ${round.contestants.length} · <b>总耗时</b> ${durationMs != null ? formatMs(durationMs) : "—"}
    <br /><b>权限</b> ${PERMISSION_LABEL[round.permissionMode]} ·
    <b>裸机</b> ${round.bareMode ? "是" : "否"} · <b>思考等级</b> ${EFFORT_LABEL[round.effort]}
  </p>

  <h2>计分板</h2>
  <table>
    <thead><tr><th>选手</th><th>状态</th><th>用时</th><th>输出 token</th><th>轮次</th><th>diff</th><th>文件</th></tr></thead>
    <tbody>${rows}</tbody>
  </table>

  <h2>各选手详情</h2>
  ${sections}

  ${judgeSection}

  <p class="meta dim">由 codeg Agent PK 竞技场生成</p>
</body>
</html>`
}

function diffStats(diff: string | null): { added: number; removed: number } {
  let added = 0
  let removed = 0
  if (!diff) return { added, removed }
  for (const line of diff.split("\n")) {
    if (line.startsWith("+") && !line.startsWith("+++")) added += 1
    else if (line.startsWith("-") && !line.startsWith("---")) removed += 1
  }
  return { added, removed }
}
