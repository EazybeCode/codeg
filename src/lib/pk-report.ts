/**
 * 回合报告导出——把一场 PK 渲染成自包含 HTML 单文件。
 *
 * 纯前端拼装:数据全部来自 store(设置、选手、计分) + 已抓取的 diff;
 * 文件树由调用方用 getFileTree 抓取后拍平成相对路径传进来。产物不引
 * 任何外部资源,浏览器直接打开即可分享(同事 / V2EX / 存档)。
 *
 * 视觉设计:渐变 hero + 卡片式选手排名 + 颁奖台式裁判评分,
 * 暗色模式自适应,目标是「截图即可传播」。
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
  default: "逐次确认",
  acceptEdits: "自动接受",
  bypassPermissions: "全自动",
}

const EFFORT_LABEL: Record<PkRound["effort"], string> = {
  default: "默认",
  low: "低",
  medium: "中",
  high: "高",
  max: "最高",
}

const STATUS_LABEL: Record<PkContestant["status"], string> = {
  preparing: "准备中",
  connecting: "连接中",
  ready: "就绪",
  running: "运行中",
  done: "完成",
  error: "失败",
  canceled: "已取消",
}

const STATUS_COLOR: Record<PkContestant["status"], string> = {
  preparing: "#6b7280",
  connecting: "#6b7280",
  ready: "#3b82f6",
  running: "#3b82f6",
  done: "#22c55e",
  error: "#ef4444",
  canceled: "#f59e0b",
}

/** Agent 首字母 emoji 映射——用 emoji 而非真实图标避免外链。 */
const AGENT_EMOJI: Record<string, string> = {
  claude_code: "🤖",
  codex: "🧠",
  open_code: "🔧",
  gemini: "♊",
  open_claw: "🦅",
  cline: "👁",
  deepseek: "🔍",
  qoder: "⚡",
  kimi: "🌙",
  grok: "🎯",
  cursor: "🖱",
  pi: "🥧",
}

function agentEmoji(agentType: string): string {
  return AGENT_EMOJI[agentType] ?? "🤖"
}

/** diff 文本 → 带红绿标注的 <pre> 行。 */
function renderDiff(diff: string | null): string {
  if (!diff) return '<p class="dim">（无 diff 数据）</p>'
  if (diff.trim() === "")
    return '<p class="dim">（无可比较内容:选手未改动工作区）</p>'
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
  if (files.length === 0) return '<p class="dim">（没有产出文件）</p>'
  return (
    '<ul class="files">' +
    files.map((f) => `<li>${esc(f)}</li>`).join("") +
    "</ul>"
  )
}

function diffStats(diff: string | null): {
  added: number
  removed: number
} {
  let added = 0
  let removed = 0
  if (!diff) return { added, removed }
  for (const line of diff.split("\n")) {
    if (line.startsWith("+") && !line.startsWith("+++")) added += 1
    else if (line.startsWith("-") && !line.startsWith("---")) removed += 1
  }
  return { added, removed }
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

  const statusText = finished
    ? "已结束"
    : round.status === "ready"
      ? "就绪"
      : "进行中"

  // ── 选手卡片 ──
  const contestantCards = round.contestants
    .map((c) => {
      const stats = diffStats(c.diff)
      const fileCount = (filesByAgent[c.agentType] ?? []).length
      const color = STATUS_COLOR[c.status]
      const emoji = agentEmoji(c.agentType)
      const label = esc(getAgentLabel(c.agentType))
      const modelLine =
        c.selectedModel != null ? ` · ${esc(c.selectedModel)}` : ""
      const effortLine =
        c.selectedEffort != null ? ` · ${esc(c.selectedEffort)}` : ""
      return `
      <div class="card">
        <div class="card-header">
          <span class="agent-emoji">${emoji}</span>
          <span class="agent-name">${label}</span>
          <span class="badge" style="background:${color}1a;color:${color}">${STATUS_LABEL[c.status]}</span>
        </div>
        <div class="card-stats">
          <div class="stat"><span class="stat-val">${c.durationMs != null ? formatMs(c.durationMs) : "—"}</span><span class="stat-label">用时</span></div>
          <div class="stat"><span class="stat-val">${c.usage ? c.usage.outputTokens : "—"}</span><span class="stat-label">输出 token</span></div>
          <div class="stat"><span class="stat-val">${c.usage ? c.usage.turnCount : "—"}</span><span class="stat-label">轮次</span></div>
          <div class="stat"><span class="stat-val add-text">+${stats.added}</span><span class="stat-label">新增行</span></div>
          <div class="stat"><span class="stat-val del-text">−${stats.removed}</span><span class="stat-label">删除行</span></div>
          <div class="stat"><span class="stat-val">${fileCount}</span><span class="stat-label">文件</span></div>
        </div>
        <div class="card-meta">${modelLine}${effortLine}</div>
        <details class="card-detail">
          <summary>产出文件 & Diff</summary>
          <h4>产出文件 (${fileCount})</h4>
          ${renderFiles(filesByAgent[c.agentType] ?? [])}
          <h4>Diff</h4>
          ${renderDiff(c.diff)}
        </details>
      </div>`
    })
    .join("")

  // ── 裁判颁奖台 ──
  const judgeSection =
    round.judgeResult != null && round.judgeResult.scores.length > 0
      ? (() => {
          const sorted = round.judgeResult.scores
            .slice()
            .sort((a, b) => a.rank - b.rank)
          const podium = sorted
            .slice(0, 3)
            .map((s) => {
              const medal = s.rank === 1 ? "🥇" : s.rank === 2 ? "🥈" : "🥉"
              const barWidth = Math.max(s.score, 5)
              const scoreColor =
                s.score >= 80
                  ? "#22c55e"
                  : s.score >= 60
                    ? "#f59e0b"
                    : s.score >= 40
                      ? "#f97316"
                      : "#ef4444"
              return `
            <div class="podium-item rank-${s.rank}">
              <span class="medal">${medal}</span>
              <span class="agent-emoji">${agentEmoji(s.agentType)}</span>
              <span class="agent-name">${esc(getAgentLabel(s.agentType))}</span>
              <div class="score-bar-wrap">
                <div class="score-bar" style="width:${barWidth}%;background:${scoreColor}"></div>
              </div>
              <span class="score-num" style="color:${scoreColor}">${s.score}</span>
            </div>`
            })
            .join("")

          const restRows = sorted
            .slice(3)
            .map(
              (s) => `
            <tr>
              <td class="rank-cell">#${s.rank}</td>
              <td class="agent-cell"><span class="agent-emoji small">${agentEmoji(s.agentType)}</span> ${esc(getAgentLabel(s.agentType))}</td>
              <td class="score-cell">${s.score}</td>
              <td class="comment-cell">${esc(s.comment)}</td>
            </tr>`
            )
            .join("")

          const restTable =
            sorted.length > 3
              ? `<table class="judge-rest">
            <thead><tr><th>排名</th><th>选手</th><th>分数</th><th>点评</th></tr></thead>
            <tbody>${restRows}</tbody>
          </table>`
              : ""

          // 点评详情(前 3 名也展开)
          const topComments = sorted
            .slice(0, 3)
            .map(
              (s) => `<div class="judge-comment">
              <span class="medal">${s.rank === 1 ? "🥇" : s.rank === 2 ? "🥈" : "🥉"}</span>
              <b>${esc(getAgentLabel(s.agentType))}</b> <span class="score-inline">${s.score} 分</span>
              <p>${esc(s.comment)}</p>
            </div>`
            )
            .join("")

          return `
      <section class="judge-section">
        <h2 class="section-title">⚖ 裁判裁决</h2>
        <p class="judge-summary">${esc(round.judgeResult.summary)}</p>
        <div class="podium">${podium}</div>
        <div class="judge-comments">${topComments}</div>
        ${restTable}
      </section>`
        })()
      : ""

  // ── 计分板表格(紧凑概览) ──
  const scoreboardRows = round.contestants
    .map((c) => {
      const stats = diffStats(c.diff)
      const fileCount = (filesByAgent[c.agentType] ?? []).length
      return `<tr>
        <td class="agent-cell"><span class="agent-emoji small">${agentEmoji(c.agentType)}</span> ${esc(getAgentLabel(c.agentType))}</td>
        <td><span class="badge" style="background:${STATUS_COLOR[c.status]}1a;color:${STATUS_COLOR[c.status]}">${STATUS_LABEL[c.status]}</span></td>
        <td>${c.durationMs != null ? formatMs(c.durationMs) : "—"}</td>
        <td>${c.usage ? c.usage.outputTokens : "—"}</td>
        <td>${c.usage ? c.usage.turnCount : "—"}</td>
        <td><span class="add-text">+${stats.added}</span> <span class="del-text">−${stats.removed}</span></td>
        <td>${fileCount}</td>
      </tr>`
    })
    .join("")

  return `<!doctype html>
<html lang="zh-CN">
<head>
<meta charset="utf-8" />
<meta name="viewport" content="width=device-width, initial-scale=1" />
<title>PK 报告 · ${esc(round.task)}</title>
<style>
  :root { color-scheme: light dark; }
  * { box-sizing: border-box; }
  body {
    font: 14px/1.6 -apple-system, "Segoe UI", Roboto, "PingFang SC", sans-serif;
    margin: 0; padding: 0; background: #f4f4f5; color: #18181b;
  }
  @media (prefers-color-scheme: dark) {
    body { background: #0a0a0b; color: #e4e4e7; }
  }
  .container { max-width: 960px; margin: 0 auto; padding: 0 0 48px; }

  /* ── Hero ── */
  .hero {
    background: linear-gradient(135deg, #6366f1 0%, #8b5cf6 40%, #d946ef 100%);
    padding: 48px 32px 40px; color: #fff;
  }
  @media (prefers-color-scheme: dark) {
    .hero { background: linear-gradient(135deg, #4338ca 0%, #6d28d9 40%, #86198f 100%); }
  }
  .hero-label { font-size: 13px; letter-spacing: 2px; text-transform: uppercase; opacity: 0.8; }
  .hero h1 { font-size: 28px; margin: 8px 0 16px; line-height: 1.3; font-weight: 700; }
  .hero-meta { display: flex; flex-wrap: wrap; gap: 16px 24px; font-size: 13px; opacity: 0.9; }
  .hero-meta span { display: inline-flex; align-items: center; gap: 4px; }
  .hero-meta b { font-weight: 600; }

  /* ── 通用 section ── */
  .section { padding: 0 24px; margin-top: 32px; }
  .section-title { font-size: 18px; font-weight: 700; margin: 0 0 16px; }

  /* ── 计分板表格 ── */
  .scoreboard { width: 100%; border-collapse: collapse; font-size: 13px; }
  .scoreboard th, .scoreboard td {
    border-bottom: 1px solid #e4e4e7; padding: 10px 12px; text-align: left;
  }
  @media (prefers-color-scheme: dark) {
    .scoreboard th, .scoreboard td { border-color: #27272a; }
  }
  .scoreboard th { font-weight: 600; color: #71717a; font-size: 12px; text-transform: uppercase; letter-spacing: 0.5px; }
  .scoreboard tr:hover td { background: #f4f4f5; }
  @media (prefers-color-scheme: dark) { .scoreboard tr:hover td { background: #18181b; } }
  .agent-cell { font-weight: 600; white-space: nowrap; }
  .agent-emoji { font-size: 18px; margin-right: 4px; }
  .agent-emoji.small { font-size: 14px; }
  .badge { display: inline-block; padding: 2px 8px; border-radius: 999px; font-size: 11px; font-weight: 600; }

  /* ── 选手卡片网格 ── */
  .cards { display: grid; grid-template-columns: repeat(auto-fill, minmax(280px, 1fr)); gap: 16px; }
  .card {
    background: #fff; border-radius: 16px; padding: 20px;
    box-shadow: 0 1px 3px #0001, 0 1px 2px #0001;
    transition: box-shadow 0.2s;
  }
  .card:hover { box-shadow: 0 4px 12px #0002; }
  @media (prefers-color-scheme: dark) {
    .card { background: #18181b; box-shadow: 0 1px 3px #fff1; }
    .card:hover { box-shadow: 0 4px 12px #fff2; }
  }
  .card-header { display: flex; align-items: center; gap: 8px; margin-bottom: 16px; }
  .agent-name { font-weight: 700; font-size: 16px; flex: 1; }
  .card-stats { display: grid; grid-template-columns: repeat(3, 1fr); gap: 12px; margin-bottom: 8px; }
  .stat { text-align: center; }
  .stat-val { display: block; font-size: 18px; font-weight: 700; font-variant-numeric: tabular-nums; }
  .stat-label { display: block; font-size: 11px; color: #71717a; margin-top: 2px; }
  @media (prefers-color-scheme: dark) { .stat-label { color: #a1a1aa; } }
  .card-meta { font-size: 12px; color: #71717a; }
  @media (prefers-color-scheme: dark) { .card-meta { color: #a1a1aa; } }
  .add-text { color: #22c55e; }
  .del-text { color: #ef4444; }

  /* ── 折叠详情 ── */
  .card-detail { margin-top: 12px; border-top: 1px solid #e4e4e7; padding-top: 12px; }
  @media (prefers-color-scheme: dark) { .card-detail { border-color: #27272a; } }
  .card-detail summary { cursor: pointer; font-size: 13px; color: #6366f1; font-weight: 600; }
  .card-detail h4 { margin: 12px 0 6px; font-size: 12px; color: #71717a; text-transform: uppercase; letter-spacing: 0.5px; }
  @media (prefers-color-scheme: dark) { .card-detail h4 { color: #a1a1aa; } }
  .dim { color: #a1a1aa; }
  pre { overflow-x: auto; background: #f4f4f5; border-radius: 8px; padding: 12px;
        font: 12px/1.5 ui-monospace, SFMono-Regular, Menlo, monospace; }
  @media (prefers-color-scheme: dark) { pre { background: #27272a; } }
  .d.add { background: #22c55e22; } .d.del { background: #ef444422; } .d.hunk { background: #6366f122; }
  .sig { display: inline-block; width: 14px; color: #a1a1aa; user-select: none; }
  ul.files { margin: 4px 0; padding-left: 20px; font-size: 12px; }
  ul.files li { margin: 2px 0; }

  /* ── 裁判评分 ── */
  .judge-section { padding: 0 24px; margin-top: 40px; }
  .judge-summary { font-size: 14px; color: #52525b; margin-bottom: 20px; line-height: 1.7; }
  @media (prefers-color-scheme: dark) { .judge-summary { color: #a1a1aa; } }
  .podium { display: flex; flex-direction: column; gap: 12px; margin-bottom: 24px; }
  .podium-item {
    display: flex; align-items: center; gap: 12px;
    background: #fff; border-radius: 12px; padding: 16px 20px;
    box-shadow: 0 1px 3px #0001;
  }
  @media (prefers-color-scheme: dark) {
    .podium-item { background: #18181b; box-shadow: 0 1px 3px #fff1; }
  }
  .podium-item.rank-1 { border-left: 4px solid #facc15; }
  .podium-item.rank-2 { border-left: 4px solid #d1d5db; }
  .podium-item.rank-3 { border-left: 4px solid #d97706; }
  .medal { font-size: 24px; }
  .score-bar-wrap { flex: 1; height: 8px; background: #e4e4e7; border-radius: 999px; overflow: hidden; }
  @media (prefers-color-scheme: dark) { .score-bar-wrap { background: #27272a; } }
  .score-bar { height: 100%; border-radius: 999px; transition: width 0.3s; }
  .score-num { font-size: 22px; font-weight: 800; font-variant-numeric: tabular-nums; min-width: 40px; text-align: right; }
  .judge-comments { margin-bottom: 20px; }
  .judge-comment { background: #fff; border-radius: 8px; padding: 14px 16px; margin-bottom: 8px; }
  @media (prefers-color-scheme: dark) { .judge-comment { background: #18181b; } }
  .judge-comment .medal { font-size: 16px; margin-right: 4px; }
  .score-inline { color: #71717a; font-weight: 600; font-size: 13px; }
  @media (prefers-color-scheme: dark) { .score-inline { color: #a1a1aa; } }
  .judge-comment p { margin: 6px 0 0; font-size: 13px; color: #52525b; line-height: 1.6; }
  @media (prefers-color-scheme: dark) { .judge-comment p { color: #a1a1aa; } }
  .judge-rest { width: 100%; border-collapse: collapse; font-size: 13px; }
  .judge-rest th, .judge-rest td { border-bottom: 1px solid #e4e4e7; padding: 10px 12px; text-align: left; }
  @media (prefers-color-scheme: dark) { .judge-rest th, .judge-rest td { border-color: #27272a; } }
  .rank-cell { font-weight: 700; color: #71717a; white-space: nowrap; }
  @media (prefers-color-scheme: dark) { .rank-cell { color: #a1a1aa; } }
  .comment-cell { color: #52525b; }
  @media (prefers-color-scheme: dark) { .comment-cell { color: #a1a1aa; } }

  /* ── Footer ── */
  .footer { text-align: center; padding: 32px 24px 0; font-size: 12px; color: #a1a1aa; }
  .footer a { color: #6366f1; text-decoration: none; font-weight: 600; }
</style>
</head>
<body>
  <div class="container">
    <div class="hero">
      <div class="hero-label">⚔ Codeg Agent PK</div>
      <h1>${esc(round.task)}</h1>
      <div class="hero-meta">
        <span><b>${statusText}</b></span>
        <span>📅 ${new Date(round.createdAt).toLocaleString()}</span>
        <span>👥 ${round.contestants.length} 选手</span>
        <span>⏱ ${durationMs != null ? formatMs(durationMs) : "—"}</span>
        <span>🔐 ${PERMISSION_LABEL[round.permissionMode]}</span>
        <span>🧠 ${EFFORT_LABEL[round.effort]}</span>
      </div>
    </div>

    <section class="section">
      <h2 class="section-title">📊 计分板</h2>
      <table class="scoreboard">
        <thead><tr><th>选手</th><th>状态</th><th>用时</th><th>输出 token</th><th>轮次</th><th>diff</th><th>文件</th></tr></thead>
        <tbody>${scoreboardRows}</tbody>
      </table>
    </section>

    <section class="section">
      <h2 class="section-title">🏁 选手详情</h2>
      <div class="cards">${contestantCards}</div>
    </section>

    ${judgeSection}

    <div class="footer">
      由 <a href="https://github.com/nicepkg/codeg" target="_blank" rel="noopener">Codeg</a> Agent PK 竞技场生成
    </div>
  </div>
</body>
</html>`
}
