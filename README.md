# Vibe Coding Rings (Rust)

> 本项目从 [vibe-coding-rings](https://github.com/zxw1992/vibe-coding-rings) (Python) 迁移而来，使用 Rust 重写了后端和 macOS 原生菜单栏，感谢原作者的开源贡献。

Rust 版 Vibe Coding Rings — 用三圈动画环形图可视化你的 AI Coding Agent 使用数据，灵感来自 Apple Activity Rings。支持 **Claude Code**、**Codex CLI**、**Gemini CLI**、**OpenCode**，所有数据从本地文件被动读取，无需外部服务或 API 密钥。

## 三圈指标

| 环 | 指标 | 颜色 |
|----|------|------|
| 消耗 / Consume | 今日消耗 Token 数 | 红色 |
| 专注 / Focus | 今日 AI 会话活跃分钟数 | 绿色 |
| 行动 / Action | 今日工具调用次数 | 蓝色 |

## 功能特性

- 动画环形仪表盘，追踪每日目标完成度
- **多 Agent 支持** — 可切换 Claude Code、Codex CLI、Gemini CLI、OpenCode 数据源
- 7 天历史记录（含迷你环），点击可查看每小时细分数据
- macOS 原生菜单栏应用，无需打开浏览器即可查看实时统计
- 中英双语 UI，随时切换
- 零配置：直接读取本地 Agent 数据，无 API 密钥、无遥测
- 守护进程模式：后台运行，重复启动自动打开看板

## 系统要求

- macOS 10.15+
- Rust 2021 Edition (1.56+)
- 至少安装一个支持的 AI Coding Agent：Claude Code (`~/.claude/`)、Codex CLI (`~/.codex/`)、Gemini CLI (`~/.gemini/`)、OpenCode (`~/.opencode/`)

## 构建与运行

**开发模式（前台运行）：**
```bash
cargo build --release
./target/release/vibe-coding-rings --foreground
```

**守护进程模式：**
```bash
./target/release/vibe-coding-rings
# 后台运行，自动打开 http://localhost:9876
# 再次运行会直接打开浏览器看板
```

**构建 macOS .app：**
```bash
make app
# 输出: dist/Vibe Coding Rings.app
```

**构建 DMG 安装包：**
```bash
make dmg
# 输出: dist/Vibe Coding Rings-1.1.0.dmg
```

## 配置目标

默认目标：每天 **100 万 Token / 120 分钟专注 / 50 次工具调用**。

通过 Web UI 的 "Daily Goals" 面板调整，修改即时生效，保存至 `~/Library/Application Support/VibeCodingRings/config.json`。菜单栏也会实时更新。

## 项目结构

```
src/
  main.rs            入口：守护进程 fork + 启动 Web 服务 + 菜单栏
  config.rs          Goals 配置加载/保存
  providers.rs       AgentProvider trait + 各 Agent 实现
  data_collector.rs  聚合多 Agent 指标，带分钟级缓存
  server.rs          Axum Web 服务器 (REST API)
  menubar.rs         macOS 原生 NSStatusItem 菜单栏 (objc2)
static/
  index.html         单页应用
  style.css          深色主题
  rings.js           前端逻辑：环形图、图表、目标设置
Info.plist           macOS .app 包元数据
Makefile             构建 .app 和 .dmg 的任务
```

## API 接口

| 端点 | 方法 | 说明 |
|------|------|------|
| `/api/today` | GET | 今日指标 + 连续达标天数 + 当前目标 |
| `/api/history` | GET | 最近 7 天历史指标 |
| `/api/goals` | GET/POST | 获取或设置每日目标 |
| `/api/agents` | GET/POST | 获取或设置启用的 Agent |
| `/api/lang` | POST | 切换语言 (zh/en) |
| `/api/hourly` | GET | 按小时细分数据 (`?metric=tokens\|tools\|focus&d=YYYY-MM-DD`) |

## 数据采集方式

各 Agent Provider 只读取本地文件（只读），数据不会离开本机：

| Agent | 会话文件 | 专注时长来源 |
|-------|---------|-------------|
| Claude Code | `~/.claude/projects/**/*.jsonl` | `~/.claude/history.jsonl` |
| Codex CLI | `~/.codex/**/*.jsonl` | `~/.codex/history.jsonl` |
| Gemini CLI | `~/.gemini/**/*.jsonl` | `~/.gemini/history.jsonl` |
| OpenCode | `~/.opencode/**/*.jsonl` | `~/.opencode/history.jsonl` |

## 技术栈

- **后端**: Rust + Axum + Tokio
- **macOS 菜单栏**: objc2-app-kit (NSStatusItem + 自定义 NSView 绘制环形图)
- **前端**: 原生 HTML/CSS/JS

## 从 Python 版本迁移的变化

| 特性 | [Python 版](https://github.com/zxw1992/vibe-coding-rings) | Rust 版 (本项目) |
|------|----------|--------|
| 语言 | Python 3.9+ | Rust 2021 |
| 平台 | macOS / Windows / Linux | macOS |
| Web 框架 | FastAPI + Uvicorn | Axum + Tokio |
| 菜单栏 | rumps (macOS) / pystray (跨平台) | 原生 objc2 NSStatusItem |
| 端口 | 8765 | 9876 |
| 运行方式 | `python main.py` | 单二进制文件，支持守护进程模式 |
| 分发 | pip install | .app bundle / DMG |
| 前端 | 复用原项目 `static/` | 复用原项目 `static/` |

## 致谢

- 原项目 [vibe-coding-rings](https://github.com/zxw1992/vibe-coding-rings) — 概念设计、前端 UI、数据采集逻辑
- Apple Activity Rings — 环形图灵感

## License

MIT
