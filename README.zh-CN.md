<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/logo-dark.png">
  <img src="assets/logo-light.png" alt="Clift" width="360">
</picture>

**把本机截图直接粘给 SSH 服务器上的 Claude Code、Codex 等编程 Agent。**

不用上传图床，不用改 SSH 配置，也不用在服务器上跑常驻进程。

[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![CI](https://github.com/leazoot/clift/actions/workflows/ci.yml/badge.svg)](https://github.com/leazoot/clift/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/leazoot/clift?include_prereleases)](https://github.com/leazoot/clift/releases)

[English](README.md) · 简体中文

</div>

---

## Clift 是做什么的？

你在服务器上通过 SSH 使用 Claude Code、Codex、Gemini CLI 或其他命令行 Agent。

文本可以直接粘贴，但截图不行。

图片在你笔记本的剪贴板里，Agent 却运行在另一台机器上。通常只能先保存图片、上传到服务器，再把路径发给 Agent。

Clift 把这几步合成一个快捷键：

```text
截图
  ↓
Cmd+Shift+V
  ↓
通过 SSH / SFTP 发送
  ↓
终端里自动出现文件路径
  ↓
Agent 直接读取
```

例如：

```text
Please inspect this file: '/home/dev/.cache/clift/inbox/2026-09-05/2a07…/clipboard.png'
```

Fast Mode 下文件直接通过你现有的 SSH 连接发送。

服务器不需要安装 Clift，也不经过 Relay。

---

# 快速开始

推荐先用 **Fast Mode**。

如果你已经有能正常 `ssh` 登录的服务器，几分钟就能用起来。

## 1. 安装

笔记本端目前支持 macOS 和 Windows。

### macOS

```bash
curl -fsSL https://raw.githubusercontent.com/leazoot/clift/main/install.sh | sh
```

### Windows

在 PowerShell 中运行：

```powershell
irm https://raw.githubusercontent.com/leazoot/clift/main/install.ps1 | iex
```

安装包会和对应的 `SHA256SUMS` 一起下载，校验失败不会安装，也不需要 `sudo`，完成后自动进入 `clift setup`。

---

## 2. 配置 SSH 目标

假设你的 `~/.ssh/config` 里已经有：

```sshconfig
Host core
    HostName 10.0.0.8
    User dev
```

运行：

```bash
clift setup core
```

依次检查：

* SSH 是否能连接
* SFTP 是否可用
* 远端 inbox 是否能创建
* 测试文件能否上传和删除

全部通过后才会保存配置。

Clift 不修改你的 `~/.ssh/config`，也不会读取 SSH 私钥。连接、认证和 `known_hosts` 校验仍然由系统 OpenSSH 负责。

---

## 3. 注册快捷键

```bash
clift hotkey --install
```

默认快捷键：

| 平台      | 快捷键           |
| ------- | ------------- |
| macOS   | `Cmd+Shift+V` |
| Windows | `Ctrl+Alt+V`  |

快捷键助手会注册为登录时启动。

macOS 第一次使用时会请求「辅助功能」权限，因为 Clift 需要把生成的文本输入到当前窗口。

---

## 4. 粘贴截图

先截图，然后把焦点放回正在 SSH 的终端。

按：

```text
Cmd+Shift+V
```

Clift 会把图片传到服务器，然后在当前窗口输入：

```text
Please inspect this file: '/home/dev/.cache/clift/inbox/2026-09-05/2a07…/clipboard.png'
```

Agent 直接读取这个文件即可。

### 普通文本不会受影响

如果剪贴板里是纯文本，Clift 不会上传任何东西，快捷键也不会做任何事。文本照常用 `Cmd+V` 粘贴。

---

# Fast Mode 是怎么工作的？

Fast Mode 没有额外的服务。

```text
┌──────────────┐         SSH / SFTP         ┌──────────────┐
│   Laptop     │ ─────────────────────────▶ │    Server    │
│              │                            │              │
│  Clipboard   │                            │ inbox/image  │
└──────────────┘                            └──────────────┘
```

Clift 使用的就是你已经配置好的：

```text
ssh
sftp
~/.ssh/config
known_hosts
SSH Agent / 系统认证方式
```

服务器上不需要：

* 安装 Clift
* 安装插件
* 修改 SSH 配置
* 开端口
* 跑 daemon
* 部署 Relay

远端文件放在当前用户目录下的私有 inbox 中，目录权限为 `0700`，文件权限为 `0600`。

Clift 只在你主动粘贴时读取一次剪贴板，不监听剪贴板，也不保存剪贴板历史。

---

# Fast Mode 和 Universal Mode

Clift 有两种传输方式。

|             | **Fast Mode**           | **Universal Mode**        |
| ----------- | ----------------------- | ------------------------- |
| 适合          | 已经配置好的 SSH 服务器          | 经常切换服务器、临时机器或大量远端环境       |
| 目标怎么确定      | Clift 使用已配置的 SSH target | Token 被粘到哪台服务器，就由哪台服务器取文件 |
| 传输          | SSH / SFTP 直连           | 本地加密 → Relay → 服务器取回      |
| Relay       | 不需要                     | 需要                        |
| 服务器安装 Clift | 不需要                     | 需要                        |
| 修改 SSH 配置   | 不需要                     | 不需要                       |
| 服务器常驻进程     | 不需要                     | 不需要                       |
| 推荐场景        | 默认选择                    | 需要“当前会话决定目标”时             |

如果你的服务器本来就在 `~/.ssh/config` 里，优先使用 Fast Mode。

Universal Mode 解决的是另一个问题：

> 我不想让本机 Clift 预先知道这次到底要发到哪台服务器。

---

# Universal Mode

Fast Mode 由本机选择目标。

Universal Mode 则由**当前终端会话**决定目标。

你按下快捷键后，本机会生成一条类似这样的内容：

```text
Attachment: clift fetch 'clift://v1/…'
```

把这行内容粘到哪台服务器，那台服务器就可以取走附件。

本机不需要预先登记这台服务器。

---

## 1. 准备 Relay

Universal Mode 需要一个 Relay。

Relay 只负责暂存加密后的附件。你可以自己运行 `clift-relayd`，也可以部署到自己的 Cloudflare 账户：

[![Deploy to Cloudflare](https://deploy.workers.cloudflare.com/button)](https://deploy.workers.cloudflare.com/?url=https://github.com/leazoot/clift/tree/main/relay/cloudflare)

得到地址后，例如：

```text
https://clift-relay.<you>.workers.dev
```

---

## 2. 配置服务器

Universal Mode 不要求你在**笔记本上登记服务器**，但接收附件的服务器需要安装 `clift`，并知道 Relay 地址。

### 让Agent来安装和配置（推荐）
把下面这段发给运行在服务器上的 Agent，并替换 Relay 地址：

```text
Set up Clift on this server so I can paste screenshots to you.

RELAY_URL: https://clift-relay.<you>.workers.dev

Follow https://raw.githubusercontent.com/leazoot/clift/main/install.md exactly:
fetch it, work through its TODO list in order, stop and show me the error if a
step fails, and report as its last step says.
```


### 手动配置

Linux / macOS：

```bash
curl -fsSL https://raw.githubusercontent.com/leazoot/clift/main/install.sh | sh -s -- --no-setup

clift config set relay.url https://clift-relay.<you>.workers.dev

clift doctor
```

如果希望 Agent 自动识别 Clift Token，再把对应说明加入 Agent 的指令文件。

Claude Code：

```bash
curl -fsSL https://raw.githubusercontent.com/leazoot/clift/main/integrations/agents/clift.md >> CLAUDE.md
```

Codex：

```bash
curl -fsSL https://raw.githubusercontent.com/leazoot/clift/main/integrations/agents/clift.md >> AGENTS.md
```

其他 Agent 可以追加到它实际读取的指令文件，例如：

```text
GEMINI.md
AGENTS.md
CLAUDE.md
```

Relay 地址不会放进 Token，所以每台接收服务器都需要配置一次 Relay 地址。

---

## 3. 粘贴

截图以后，在 SSH 会话里按快捷键。

Universal Mode 不会直接上传到某台 SSH 主机，而是输入：

```text
Attachment: clift fetch 'clift://v1/…'
```

服务器执行：

```bash
clift fetch 'clift://v1/…'
```

成功后会返回服务器上的文件路径，Agent 就可以读取。

不需要告诉本机 Clift 当前 SSH 到的是哪台机器。

---

## Claude Code Hook （可选）

如果使用 Claude Code，可以安装专用 Hook：

[Claude Code integration](integrations/claude-code/README.md)

安装以后，附件 Token 在提交时就可以被取回，不需要 Claude 先判断并执行一次 `clift fetch`。

---

# 从服务器把图片带回来

Universal Mode 也支持反方向传输。

服务器上：

```console
$ clift copy build/report.png
clift://v1/…
```

复制返回的 Token：

```text
clift://v1/…
```

然后在笔记本上按 Clift 快捷键。

图片会进入本机剪贴板，可以直接粘到浏览器、聊天软件或其他应用。

如果本机能够方便地直接使用 `scp` / `sftp` 拉文件，那么直接使用它们通常更简单。这个功能主要用于你已经处在终端工作流里、不想另外处理文件传输的时候（说白点就是没多大用处）。

---

# Universal Mode 的工作原理

```text
 Laptop                         Relay                         Server
─────────                     ─────────                     ─────────

Clipboard
    │
    │  XChaCha20-Poly1305
    ▼
Ciphertext ───────────────────▶ store
                                  │
                                  │ ciphertext
                                  ▼
                              clift fetch
                                  │
                                  ▼
                               decrypt
                                  │
                                  ▼
                                inbox/


Encryption key ─────── inside pasted Token ───────────────▶ Server

                    key is never sent to Relay
```

每个附件都会使用新的 **XChaCha20-Poly1305** 密钥和 nonce。

Relay 拿到的是加密后的数据。附件内容、文件名和媒体类型都在加密数据内部。

密钥位于 Token 的 URL fragment 中，不会发送给 Relay。

注：附件默认只能成功取回一次；未取回的对象会在过期后失效。


---

# 其他安装方式

### Homebrew

```bash
brew install leazoot/clift/clift
```

### cargo-binstall

```bash
cargo binstall --git https://github.com/leazoot/clift clift-cli
```

### Releases

直接从：

[GitHub Releases](https://github.com/leazoot/clift/releases)

下载对应平台的发布包。

### 从源码构建

需要 Rust 1.95 或更高版本：

```bash
git clone https://github.com/leazoot/clift.git
cd clift
cargo build --release
```

Windows 的 Scoop 清单位于：

[`packaging/`](packaging/)

---

# 常用命令

| 命令                                  | 作用                           |
| ----------------------------------- | ---------------------------- |
| `clift setup`                       | 交互式配置                        |
| `clift setup <ssh-host>`            | 验证并保存一个 Fast Mode SSH target |
| `clift paste`                       | 处理当前剪贴板内容                    |
| `clift paste --copy`                | 生成内容并复制到剪贴板                  |
| `clift paste --inject`              | 将生成的内容输入当前窗口                 |
| `clift send [files…]`               | Fast Mode 发送文件               |
| `clift send [files…] --to <target>` | 指定 Fast Mode target          |
| `clift fetch '<token>'`             | Universal Mode 取回附件          |
| `clift fetch '<token>' --copy`      | 取回图片并放进剪贴板                   |
| `clift copy <file…>`                | 把服务器文件封装成可带回本机的 Token        |
| `clift hotkey --install`            | 安装全局快捷键助手                    |
| `clift doctor`                      | 检查当前配置和连接                    |
| `clift status`                      | 查看当前状态                       |
| `clift config`                      | 查看或修改配置                      |
| `clift clean`                       | 清理 Clift 文件                  |

命令支持 `--json` 输出，错误也有固定退出码，方便脚本和 Agent 调用。

---

# 配置文件

配置位置：

### macOS / Linux

```text
~/.config/clift/config.toml
```

### Windows

```text
%APPDATA%\Clift\config.toml
```

一个 Universal Mode 配置可能是：

```toml
mode = "universal"

[relay]
url = "https://clift-relay.<you>.workers.dev"
max_bytes = "8MiB"
ttl = "5m"

[hotkey]
combination = "cmd+shift+v"
```

配置文件中不保存附件密钥。

---

# 排查问题

先运行：

```bash
clift doctor
```

它会检查当前模式需要的组件，并指出失败的位置。

### SSH 能登录，但 Clift 不能发送

重新验证目标：

```bash
clift setup core
```

Clift 同时需要 SSH 和 SFTP 正常工作。

### SSH Host Key 发生变化

Clift 不会自动绕过 Host Key Verification。

请先确认服务器 Host Key 的变化是否可信，再按照你平时管理 OpenSSH 的方式处理。

### macOS 按快捷键后要再按一次 Cmd+V

说明快捷键助手没有「辅助功能」权限，只能把内容放进剪贴板。

权限要给 `clift` 程序本身，不是终端。`clift hotkey --install` 输出的 `program:` 就是它的路径，默认是 `~/.local/bin/clift`。

在这里加上它：

```text
系统设置
→ 隐私与安全性
→ 辅助功能
```

然后重新运行：

```bash
clift hotkey --install
```

### Universal Mode 无法取回附件

检查两端 Relay 配置：

```bash
clift status
clift doctor
```

Token 只能使用一次，并且会过期。

---

# 为什么不是直接用 scp？

Clift 解决的是另一个场景：

> 图片刚刚进入剪贴板，而你的手还在 Claude Code / Codex 的 SSH 会话里。

它省掉的是：

```text
保存图片
→ 找文件
→ 想路径
→ scp
→ 找远端路径
→ 再把路径发给 Agent
```

换成：

```text
截图
→ 快捷键
```

---

# 为什么有两种模式？

Fast Mode 和 Universal Mode 不是“简单版”和“完整版”。

它们解决的是两种不同的目标选择方式。

### Fast Mode

本机知道目标：

```text
我要发给 core。
```

所以可以直接走 SSH / SFTP。

### Universal Mode

本机不知道、也不需要知道目标：

```text
我把 Token 粘到哪台机器，就由哪台机器取。
```

所以需要 Relay 临时保存密文。

大多数情况下，**Fast Mode 就够了**。

只有当“当前终端会话决定目标”比“本机预先配置目标”更方便时，再使用 Universal Mode。

---

# 隐私

Clift：

* 没有账号系统
* 没有遥测
* 不监听剪贴板
* 不保存剪贴板历史
* Fast Mode 不使用任何第三方服务
* 不提供写死在客户端里的公共 Relay

---

# 参与开发

开发和贡献说明：

[CONTRIBUTING.md](CONTRIBUTING.md)

发现安全问题：

[SECURITY.md](SECURITY.md)

安全模型：

[THREAT_MODEL.md](THREAT_MODEL.md)

## 友情链接

- [LINUX DO](https://linux.do/)

---

# License

Apache-2.0

见 [LICENSE](LICENSE) 和 [NOTICE](NOTICE)。
