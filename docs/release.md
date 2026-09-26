# 发布流程 · 签名、清单与托管（M4.2 · 设计 §13 · 决断 1）

> 化境的自动更新是**无服务器**形态：客户端能力（签名校验、下载、重启安装）内置，
> 更新清单是一个静态 `latest.json`，托管在任意静态站点或 GitHub Releases 上皆可。
> 没有更新服务、没有差量更新、没有强制更新——静态清单 + 全量包对 v1 自用足够。

## 1. 签名密钥管理

更新包用 minisign 签名（Tauri 官方机制）：公钥内置于客户端（`src-tauri/tauri.conf.json`
的 `plugins.updater.pubkey`），私钥只存在于发布者手里。

- **本机密钥位置**：`%USERPROFILE%\.tauri\huajing.key`（私钥，**绝不入库**）与
  `huajing.key.pub`（公钥，已进 tauri.conf.json）。
- **生成**（仅在换新密钥时）：`pnpm tauri signer generate -w "%USERPROFILE%\.tauri\huajing.key" --ci`
  （`--ci` = 无口令；换密钥后要改 tauri.conf.json 的 pubkey 并**重发全量**——老客户端
  不认新签名的包）。
- **构建时签名**要读环境变量：
  - `TAURI_SIGNING_PRIVATE_KEY_PATH` = `%USERPROFILE%\.tauri\huajing.key`（或
    `TAURI_SIGNING_PRIVATE_KEY` 直接给私钥内容）；
  - `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`：无口令密钥留空即可。
- **私钥丢失 = 无法再发更新**（老客户端只认这把公钥）。请另行备份（密码管理器/离线介质）。

`.gitignore` 不需要特判：密钥从未放进仓库目录。CI 的 Release 工作流**不签名**——
签名与清单生成是发布者在本地机器（持有私钥）上完成的显式步骤，见下文第 4 节。

## 2. 版本号三处同步

发布前把版本统一 bump 到目标版本（有单测断言三处相等，漂移在 `cargo test` 即红）：

| 文件 | 字段 |
|---|---|
| `src-tauri/tauri.conf.json` | `version` |
| `src-tauri/Cargo.toml` | `[package] version` |
| `package.json` | `version` |

## 3. 构建安装包

```bash
pnpm tauri build            # NSIS + MSI 落 src-tauri/target/release/bundle/
```

产物：

- `bundle/nsis/huajing_<版本>_x64-setup.exe`（NSIS 安装器，**更新包就用它**）；
- `bundle/msi/huajing_<版本>_x64_zh-CN.msi`（WiX 备选）。

## 4. 签名更新包并生成 latest.json

签名是对**安装器文件本体**做的（minisign detached signature），`.sig` 文件内容整段
放进清单：

```bash
export TAURI_SIGNING_PRIVATE_KEY_PATH="$USERPROFILE/.tauri/huajing.key"
pnpm tauri signer sign "src-tauri/target/release/bundle/nsis/huajing_0.4.0_x64-setup.exe"
# → 同目录生成 huajing_0.4.0_x64-setup.exe.sig
```

`latest.json`（Tauri v2 静态清单格式，与平台一节的 key 严格对齐——客户端按
`windows-x86_64-nsis` → `windows-x86_64` 的顺序回退查找）：

```json
{
  "version": "0.4.0",
  "notes": "0.4.0 定版：……（CHANGELOG 定版段摘要）",
  "pub_date": "2026-10-01T12:00:00Z",
  "platforms": {
    "windows-x86_64-nsis": {
      "signature": "<huajing_0.4.0_x64-setup.exe.sig 的文件内容，原样整段>",
      "url": "https://<托管域名>/huajing/0.4.0/huajing_0.4.0_x64-setup.exe"
    }
  }
}
```

要点：

- `version` 必须**大于**当前版本（semver 比较），相等或更低不会触发更新；
- `signature` 是 `.sig` 的**文本内容**（不是 base64 再编码，不是文件路径）；
- `pub_date` 是 ISO-8601 UTC 时间戳。

## 5. 托管与端点

- 把 `latest.json` 与安装器上传到任意静态托管（GitHub Releases / R2 / OSS 皆可）；
- 客户端 `settings.toml` 的 `[updater] endpoint` 填 **latest.json 的完整 URL**，
  `enabled = true` 打开（缺省关——未启用时设置页只提示不报错）：

  ```toml
  [updater]
  enabled = true
  endpoint = "https://<托管域名>/huajing/latest.json"
  ```

- URL 里可用模板变量：`{{current_version}}` / `{{target}}` / `{{arch}}`（按需）；
- **正式构建只接受 https 端点**（updater 插件的传输安全校验；http 在 debug 构建下
  仅告警放行——这正是本地构造用例能跑 127.0.0.1 的原因）；
- 装机后的更新流程：设置页「关于与更新」→ 检查更新 → 下载（进度条）→ 下载完成即
  签名校验 → 启动 NSIS 安装器（passive 进度条 + 装完自动重启，参数 `/P /R`）。

## 6. 发布检查清单

1. CHANGELOG `[x.y.z]` 定版段写好；
2. 版本号三处 bump + `cargo test`（版本一致性断言）+ `pnpm build` 双绿；
3. tag `v<x.y.z>` 推送（GitHub Actions 出未签名安装包 + 草稿 Release，人工 Publish）；
4. 本地 `pnpm tauri build` + `pnpm tauri signer sign`（持私钥的机器）；
5. 上传安装器 + `latest.json` 到托管；老版本客户端真机走一遍「检查 → 下载 → 重启」；
6. `docs/plan/m4.md` 进度日志记一笔。

## 7. 本地构造用例（验证更新闭环，不碰公网）

debug 构建允许 http 端点，因此可在本机起静态服务完整演练（M4.2 DoD 的做法）：

1. 临时把版本改成目标版（如 0.3.2，三处），`pnpm tauri build --debug --bundles nsis`；
2. `pnpm tauri signer sign` 该 debug 安装器；
3. 临时目录放 `latest.json`（url 指向本机 http 服务）+ 签名安装器，起
   `python -m http.server 8080`；
4. 老版本 exe 的 `DataHub/settings.toml` 写 `[updater] enabled=true,
   endpoint="http://127.0.0.1:8080/latest.json"`；
5. 应用内检查 → 下载 → 安装器接管 → 重启进新版本；
6. 篡改用例：把 latest.json 的 `signature` 换成一段合法格式但错误的签名（或对包
   改一个字节），客户端必须在下载后拒绝安装并报「签名校验失败」。
