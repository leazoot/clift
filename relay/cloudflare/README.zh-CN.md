# Clift 的 Cloudflare Relay

[English](README.md) · 简体中文

给没有机器跑 `clift-relayd` 的人。

这是同一个 Relay 的 Cloudflare Worker 形态:把密文保存几分钟,交出去一次,然后忘掉。
它与守护进程说同样的四条路由、给同样的拒绝、用同样的数字,而且被同一套测试约束着:
`crates/clift-relay/tests/real_relay.rs` 里的每一个场景都对着守护进程跑一遍,*再*对着真实
`workerd` 运行时里的这个 Worker 跑一遍。

[![Deploy to Cloudflare](https://deploy.workers.cloudflare.com/button)](https://deploy.workers.cloudflare.com/?url=https://github.com/leazoot/clift/tree/main/relay/cloudflare)

## 你会得到什么

一个 Worker 加一个 Durable Object,跑在免费的 Cloudflare 账户里,地址形如
`https://clift-relay.<你的子域>.workers.dev`。然后在你**发起粘贴**的那台机器上:

```console
$ clift config set relay.url https://clift-relay.<你的子域>.workers.dev
```

再在**每台跑 Agent 的机器**上各做一次,因为 Token 里带着对象和密钥,唯独不带 Relay 的地址:

```console
$ clift config set relay.url https://clift-relay.<你的子域>.workers.dev
$ # 或者只用一次:  CLIFT_RELAY_URL=https://… clift fetch '<token>'
```

就这些。`clift paste` 现在可以粘进任何 SSH 会话,不用配置 target;对面用 `clift fetch`
把附件取下来。

## 为什么只用一个 Durable Object

Durable Object 一次只处理一个事件。正是这一根线程让 Relay 唯一的硬保证,*一个对象只交给
同时到达的众多 fetch 中的恰好一个*,由构造本身成立,而不靠锁;也正是它让健康检查能说出
「现在存着多少」,因为东西只在一个地方。守护进程放在一个进程里的一切,Worker 放在一个对象里。

这个对象永远叫 `relay`。前面的 Worker 收到的每个请求都送到同一个对象。

密文按 1 MiB 一行存进对象自带的 SQLite,因为单行上限是 2 MB。一个 8 MiB 的附件就是八行。
过期在每个请求上都会检查;空闲时则由一个设在最早过期时刻的 alarm 负责。

## 限额,以及为什么免费计划够用

按 Cloudflare 自己的文档:

| | 免费计划 | Relay 需要的 |
| --- | --- | --- |
| Durable Objects | 可用,只有 SQLite 存储 | SQLite 存储 |
| 请求数 | 100,000 / 天 | 每个附件两次 |
| 存储量 | 5 GB | 不到 256 MiB,只存几分钟 |
| 行写入 | 100,000 / 天 | 每个附件约九行 |
| 请求体 | 100 MB | 默认 8 MiB |
| 每请求 CPU | 10 ms | 只是搬字节;这里不做任何加密运算 |

## 配置

名字和写法与 `clift-relayd` 完全一样,写在 `wrangler.jsonc` 里,或者命令行 `--var 名字:值`:

| 变量 | 默认 | 含义 |
| --- | --- | --- |
| `CLIFT_RELAY_MAX_BYTES` | `8MiB` | 单个对象最大多少 |
| `CLIFT_RELAY_TTL` | `5m` | 对象最长活多久(硬上限 1h) |
| `CLIFT_RELAY_MAX_TOTAL_BYTES` | `256MiB` | 同时最多存多少 |
| `CLIFT_RELAY_RATE_LIMIT` | `60` | 每来源每分钟请求数;`0` 关闭 |

配错一个值不会像守护进程那样让它起不来,Worker 总是存在的,所以它会拒绝每一个请求,并说明原因。

## 与守护进程不同的地方

- **TLS 是 Cloudflare 的。** 守护进程只说明文 HTTP,期望前面有反向代理;这里代理就是边缘节点,证书你永远看不到。
- **开始交付即算交付。** 客户端在字节到达前断开时,守护进程会把对象放回去;Worker 无从得知客户端有没有读到响应,所以一交出去就消费掉。这是往「绝不交付两次」那一侧错。
- **限流按 `CF-Connecting-IP` 计数,** 那是客户端在边缘的真实地址;守护进程按 socket 对端计数。
- **限流窗口只是一处内存。** 两者都把窗口放内存里:守护进程重启即清零,Worker 在对象空闲被逐出时清零。都刻意不持久化。

## 相同的地方

客户端能观察到的一切。路由、状态码、错误文档、每个响应上的 `Cache-Control: no-store`、
从不回显请求内容的固定文案、来自运行时 128 位随机的 22 字符 id、以及对 `DELETE`
「有没有找到」一律不说。Relay 只看见密文和 id;没有任何请求字段能装下密钥,id 在这一端生成,
客户端选不了。

## 不用按钮,自己部署

```console
$ cd relay/cloudflare
$ npm install
$ npx wrangler login          # 打开一次浏览器
$ npx wrangler deploy
```

`wrangler deploy` 会打印 URL。要在本地对着 Worker 跑契约测试,只需 `npm install`;
测试自己起 `wrangler dev`,没装的话会大声地跳过:

```console
$ CLIFT_E2E_REQUIRE_WRANGLER=1 cargo test -p clift-relay --test real_relay
```

## 它不做什么

守护进程不做的它都不做。不列举、不鉴权、无账号、无元数据、没有办法问一个对象里装着什么。
这个 URL 刻意不设鉴权:Relay 解不开它存的东西,所以没有什么值得一个账号去保护。这意味着
知道地址的人可以花掉你每天的请求配额。这就是全部的暴露面,每来源限流就是给它划的界。

## Cloudflare 多出来的一件事

Cloudflare 的边缘会在每个 Worker 前面套一层自己的机器人防护,对某些非浏览器的 HTTP 客户端,
它可能在 Worker 看到请求之前就回 `403` 与 `error code: 1010`。`clift` 本身(User-Agent 为
`clift/<版本>`)和 `curl` 都能通过。如果你用别的东西脚本化地访问自己的 Relay,拿到一个不是 JSON
的 403,那是边缘,不是 Relay。

部署与跑测试需要 Node.js 和 `wrangler`;运行 `clift` 永远不需要它们。

## 如果服务器在中国大陆

`*.workers.dev` 在中国大陆的网络里常常不可达:域名解析到的不是 Cloudflare 的地址,连接始终建不起来。
Clift 改变不了这件事:够不着 Relay 的主机兑换不了 Token,那里的 `clift fetch` 会以退出码 28 结束。

两条绕法,都不是 Clift 能替你做的:

- 给 Worker 挂一个你自己在 Cloudflare 上的域名(Workers 路由),那是另一个名字,可能在 `workers.dev`
  不可达的地方可达;
- 或者在两端都能到的机器上跑 `clift-relayd`,守护进程就是为此存在的。

依赖之前先测:在服务器上跑 `curl -sS <relay-url>/v1/health`,这就是全部的测试。
