<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/logo-dark.png">
  <img src="assets/logo-light.png" alt="Clift" width="360">
</picture>

**把截图粘贴给服务器上正在运行的编程 Agent。**

本地复制,SSH 会话里粘贴,Agent 就能读到文件。

[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![CI](https://github.com/leazoot/clift/actions/workflows/ci.yml/badge.svg)](https://github.com/leazoot/clift/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/leazoot/clift?include_prereleases)](https://github.com/leazoot/clift/releases)

[English](README.md) · 简体中文

</div>

---

你通过 SSH 在用 Claude Code、Codex 或别的命令行 Agent。你截了一张图。
剪贴板在你的笔记本上,Agent 在服务器上,`Cmd+V` 粘不出任何有用的东西。
Clift 补上这一段:不碰你的 SSH 配置,服务器上不跑常驻进程,也不在乎你用哪个终端。

```console
$ clift setup core              # 你 ~/.ssh/config 里的别名;检查 SSH 与 SFTP,然后记住它
$ clift hotkey --install        # 一个组合键,注册为登录时启动

$ # 截图,在 SSH 会话里按下这个键,会话里出现这一行:
Please inspect this file: '/home/dev/.cache/clift/inbox/2026-09-05/2a07…/clipboard.png'
```

Agent 读这个路径。服务器上什么都没装,不经过任何 Relay,文件走的是你本来就有的 `ssh` 和 `sftp`。

## 两种用法

| | **Fast Mode** | **Universal Mode** |
| --- | --- | --- |
| 适合 | 已经配好的一台服务器 | 任意多台,包括你没配置过的 |
| 发到哪台? | 你配置的那台 | 你把 token 粘进哪个会话,就是哪台 |
| 传输路径 | 你自己的 SSH 与 SFTP,直连 | 本地加密,经一个只见密文的 Relay,再 `clift fetch` |
| 服务器上需要什么 | 什么都不需要 | 同一个 `clift` 二进制,每次粘贴运行一次 |
| 第一次粘贴之前 | `clift setup <ssh-host>` | 同上,外加一个你自己跑或部署的 Relay |

两种模式由同一个组合键驱动,也都不碰纯文本。**先用 Fast Mode**:两条命令,不需要你本来没有的任何东西。
等一台配好的主机不够用了再换 Universal Mode,那个时刻就是你不想再告诉 Clift「是哪台」的时刻。

## 快速开始

### 1. 在笔记本上安装

macOS 或 Linux 上一行:

```console
$ curl -fsSL https://raw.githubusercontent.com/leazoot/clift/main/install.sh | sh
```

Windows 上在 PowerShell 里:

```powershell
PS> irm https://raw.githubusercontent.com/leazoot/clift/main/install.ps1 | iex
```

两个脚本都会把发布包连同它的 `SHA256SUMS` 一起下载,摘要不符就什么都不装,也不需要 sudo。
装完后会接着启动 `clift setup`,先问你用哪种模式,然后顺着往下问:Fast Mode 要一个 SSH 主机别名,
Universal Mode 要一个 Relay 地址(保存之前先真实往返一次)。把问题答完,下面两步就已经做完了;
命令写在这里,是为了让你看见它们做了什么。随时可以再跑一次 `clift setup`。

其他安装方式:`brew install leazoot/clift/clift`、`cargo binstall --git https://github.com/leazoot/clift clift-cli`、
[Releases](https://github.com/leazoot/clift/releases) 页面,或者用 Rust 1.95 及以上 `cargo build --release`。
Scoop 清单在 [`packaging/`](packaging/)。加 `--no-setup`(或设置 `CLIFT_NO_SETUP=1`)可以只安装、不问问题。

### 2. 指定一台服务器

```console
$ clift setup core
```

`core` 是你自己 `~/.ssh/config` 里的别名。Clift 会先把解析出来的用户、主机、端口显示给你,
等你确认,然后依次检查 SSH、检查 SFTP、创建一个私有 inbox、上传一个文件再删掉。
**全部通过之前不会写入任何配置**,而第一台配好的主机自动成为默认目标。

### 3. 注册组合键

```console
$ clift hotkey --install
```

macOS 和 Windows 上,助手注册为登录时启动并隐藏运行,不需要一直开着终端。
macOS 第一次会弹窗要「辅助功能」权限,因为「往别的应用里打字」正是这个权限管的事。

### 4. 粘贴

截图,然后在正跟服务器对话的那个终端里按下这个键(macOS 默认 `Cmd+Shift+V`,其余平台 `Ctrl+Alt+V`)。
一行字被打进会话:

```text
Please inspect this file: '/home/dev/.cache/clift/inbox/2026-09-05/2a07…/clipboard.png'
```

Agent 读它就行。**纯文本不受影响**:剪贴板里是文本时按这个键,终端照常粘贴。

Fast Mode 到这里就全部讲完了。下面是「一台服务器不够用」的时候该怎么办。

## 任意多台服务器:Universal Mode

Fast Mode 发到你配置过的那台。Universal Mode 发到**你把 token 粘进的那个会话**所在的机器,
也就是说你既不用告诉 Clift 是哪台,也根本不用配置那些机器。代价是一个 Relay,以及每台服务器装一次。

### 1. 准备一个 Relay

Relay 只保存加密后的附件几分钟,读不懂内容。可以用 `clift-relayd` 自己跑,也可以一键部署到免费的 Cloudflare 账户:

[![Deploy to Cloudflare](https://deploy.workers.cloudflare.com/button)](https://deploy.workers.cloudflare.com/?url=https://github.com/leazoot/clift/tree/main/relay/cloudflare)

把地址交给笔记本上的 `clift setup`,或者之后用 `clift config set relay.url https://clift-relay.<you>.workers.dev` 设置。

### 2. 配置每台服务器

**推荐:让 Agent 自己来。** 将来接收截图的那个 Agent 可以自己把 Clift 装好、配好。把下面这段粘给它,Relay 地址换成你的:

```text
Set up Clift on this server so I can paste screenshots to you.
RELAY_URL: https://clift-relay.<you>.workers.dev
Follow https://raw.githubusercontent.com/leazoot/clift/main/install.md exactly:
fetch it, work through its TODO list in order, stop and show me the error if a
step fails, and report as its last step says.
```

[`install.md`](install.md) 就是它照着做的说明:不用 sudo 安装、指向 Relay、跑 `clift doctor`,再往它自己的指令文件里加一小段,
让它知道 Token 来了该怎么办。这份说明人也读得懂,值得先看一遍,因为它就是你的 Agent 将要执行的命令清单。也可以直接把文件喂给它:

```console
$ curl -fsSL https://raw.githubusercontent.com/leazoot/clift/main/install.md | claude
```

**手动。** 在服务器上三条命令:

```console
$ curl -fsSL https://raw.githubusercontent.com/leazoot/clift/main/install.sh | sh -s -- --no-setup
$ clift config set relay.url https://clift-relay.<you>.workers.dev
$ curl -fsSL https://raw.githubusercontent.com/leazoot/clift/main/integrations/agents/clift.md >> CLAUDE.md
```

最后一行追加的那一段告诉 Agent 怎么处理 Token、用过的 Token 和没配 Relay 的情况。
Claude Code 还可以再进一步:装一个 [hook](integrations/claude-code/README.md),你按回车的那一刻附件就取回到本地,Claude 直接读文件,不用先决定去跑命令。
按你的 Agent 读哪个文件改成 `AGENTS.md`、`GEMINI.md` 等。Token 里带着对象和密钥,但从不带 Relay 的地址,所以每台服务器都要告诉它一次。

### 3. 粘贴

截一张图,然后在正连着服务器的终端里按 setup 时选的组合键(没改的话 macOS 是 `Cmd+Shift+V`,Windows 是 `Ctrl+Alt+V`),
或者运行 `clift paste --copy` 再粘贴。会话里会出现一行:

```text
Attachment: clift fetch 'clift://v1/…'
```

Agent 执行它,就拿到了文件。不用选目标,不用改 `ssh` 配置,不用装插件。

### 4. 把东西带回来

同一个键也能反过来用。在服务器上点名一个文件:

```console
$ clift copy build/report.png
clift://v1/…
```

用平常在终端里复制文字的方式选中那一行复制,然后在本机按下那个键。图片就在你的剪贴板里,
粘到哪儿都行。

这是给「你所在的位置连不到那台服务器」准备的。连得到的时候 `scp` 更省事,Clift 也会这么告诉你。

## Universal Mode 的工作原理

```text
 笔记本                          Relay                          服务器
 ──────                          ─────                          ──────
 剪贴板 ──加密──▶ 密文 ──▶ 保存 5 分钟 ──▶ 密文 ──解密──▶ inbox/
          密钥 ────────────────────────────────────────────▶ 密钥
                  (在你粘贴的 Token 里;从不发给 Relay)
```

- 每个附件都用一把新的 **XChaCha20-Poly1305** 密钥和 nonce 加密。
- Relay 保存它读不懂的字节,**只交出一次**,然后忘掉。
- 密钥放在 Token 的片段部分(`#…`),那是 URL 里永远不会发给服务器的部分。
- `clift fetch` 解密、逐字节校验,把文件以 `0600` 写进 `0700` 的目录。任何一步失败就什么都不写,并说明原因。

纯文本粘贴完全不受影响:该怎么粘还怎么粘。

## 和同类工具的区别

解决同一个开头问题的工具有好几个,区别在于它们要你付出什么,以及走到哪一步为止。

| | Clift | [clipssh](https://github.com/samuellawrentz/clipssh) | [clipaste](https://github.com/hqhq1025/clipaste) | [claude-ssh-image-skill](https://github.com/AlexZeitler/claude-ssh-image-skill) |
| --- | --- | --- | --- | --- |
| 监听剪贴板 | 否,按键时读一次 | 否 | **是**,常驻后台 | 否 |
| 一次配置管几台 | Universal Mode 下任意多台,且无需配置 | 你配置的那台 | 你配置的那台 | 你配置的那台 |
| 服务器上装什么 | Fast Mode 下什么都不装 | 不装 | 不装 | 一个客户端 |
| 网络形态 | 你的 SSH,或一个只见密文的 Relay | 你的 SSH | 你的 SSH | 反向隧道回本地守护进程 |
| 能把文件带回来 | 能,`clift copy` | 否 | 否 | 否 |
| 绑定某个 Agent | 否 | 否 | 否 | Claude Code |

**Clift 输在哪:** 如果你只有一台 Mac、一台服务器,并且只想要最短的安装过程,那么一个
「监听剪贴板并把图片换成路径」的工具是一条 `brew install` 外加零配置,而 Clift 是两条命令。
Universal Mode 还要你在第一次粘贴之前跑或部署一个 Relay,这是「不用说是哪台机器」的价钱。

**赢在哪:** 没有任何东西在后台读你的剪贴板;Fast Mode 下服务器上什么都不装;
真的经过 Relay 时附件是端到端加密的;文件不只能出去还能回来;
并且它不知道也不关心你用的是哪个 Agent、哪个终端。

## 命令

| 命令 | 作用 |
| --- | --- |
| `clift setup` | 首次配置问答;带 `<ssh-host>` 时验证一台 Fast Mode 主机并记住它 |
| `clift paste [--copy\|--inject]` | 发送剪贴板内容,把要粘贴的文字交给你 |
| `clift fetch '<token>' [--copy]` | 兑换 Token:打印文件路径,或把图片放上剪贴板 |
| `clift copy <file…>` | 在服务器上:密封一个文件,打印一个可以在本机粘贴的 Token |
| `clift hotkey [--install]` | 一个组合键,任何应用里都能用 |
| `clift send [files…] [--to <target>]` | Fast Mode:通过 SSH 发送文件或 `--clipboard` |
| `clift doctor` | 准确说出是什么会让发送失败 |
| `clift status` · `clift config` · `clift clean` | 查看、修改、清理 |

每个命令都有 `--json` 供程序读取,每种失败都有固定的退出码。

## 配置

macOS 和 Linux 在 `~/.config/clift/config.toml`,Windows 在 `%APPDATA%\Clift\config.toml`。只有几行,里面没有任何密钥:

```toml
mode = "universal"

[relay]
url = "https://clift-relay.<you>.workers.dev"
max_bytes = "8MiB"
ttl = "5m"

[hotkey]
combination = "cmd+shift+v"
```

## 安全,一段话

密钥不离开两端的机器。Relay 看到的只有密文和一个猜不到的 id,它无法解密,也不负责鉴别任何人。
Token 单次使用、会过期。SSH 是你自己的,原封不动。没有遥测、没有账号、没有公共 Relay,二进制里也没有写死任何地址。
Clift 不防御的情况,比如以你的身份运行的恶意进程、服务器的 root,写在 [THREAT_MODEL.md](THREAT_MODEL.md) 里。

## 参与

见 [CONTRIBUTING.md](CONTRIBUTING.md)。安全问题走 [SECURITY.md](SECURITY.md)。

## 许可证

Apache-2.0。见 [LICENSE](LICENSE) 与 [NOTICE](NOTICE)。
