# Pony Mail

一款轻量的 Windows 桌面邮件客户端：多账号 IMAP 实时收信、本地 AI 整理与摘要、标签管理，数据全部留在本机。

## 功能特性

- **多账号 IMAP 收信**：支持网易 163/126、QQ、Gmail（应用专用密码）等常见邮箱与通用 IMAP 服务器；账号密码保存在 Windows 凭据管理器。
- **实时收信**：常驻 IMAP 监听（服务器支持时走 IDLE 推送，否则自动轮询兜底），新邮件无需手动同步，并弹出系统通知。
- **系统托盘**：点关闭按钮默认最小化到托盘（可在设置中改为直接退出），后台持续收信；托盘图标在未读时显示红点、
  悬停提示未读数量，左键唤出 / 收起窗口，右键菜单可退出；另支持开机自启（静默驻留托盘，不弹窗）与单实例保护（重复启动唤醒已有窗口）。
- **本地 AI（可选）**：接入本地 Ollama（或 OpenAI 兼容接口），同步完成后自动整理邮件打标签；打开邮件自动生成摘要，命中缓存不重复推理。
- **标签与标签组**：自定义颜色、分组管理，支持按组筛选；标签组可选择交给 AI 自动整理。
- **撰写与发送**：SMTP 发送、回复 / 转发，正文渲染做了 sanitize 处理。
- **应用内检查更新**：启动自动检查（可关），发现新版本提示并从 Gitee / GitHub 下载。

> **Gmail 用户注意**：Gmail 不再支持用账号密码登录第三方客户端。请在 Google 账号开启两步验证后
> 生成 16 位「应用专用密码」，添加账号时粘贴到密码框即可（成组展示、带空格粘贴也没关系）。
> 中国大陆网络环境需先开启代理才能直连 `imap.gmail.com` / `smtp.gmail.com`。
> 删除邮件的语义已适配 Gmail：删除 = 移入回收站（30 天内可恢复），而不是网页端之外的永久删除。

## 下载安装

| 平台 | 下载页 |
| --- | --- |
| Gitee（国内推荐） | https://gitee.com/helix-byte/pony-mail/releases |
| GitHub | https://github.com/helix-byte/pony-mail/releases |

下载 `Pony.Mail_x.y.z_x64-setup.exe` 运行安装即可（Windows 10/11 自带 WebView2）。

> 应用暂未购买代码签名证书，首次安装时 Windows SmartScreen 可能提示「未知发布者」，点「更多信息 → 仍要运行」即可。每个安装包在 Release 里都附带 `.sha256` 校验文件。

## 从源码构建

要求：Node.js ≥ 20、Rust（stable）与 MSVC 工具链、Windows。

```bash
npm install
npm run tauri dev        # 开发模式（前端热更新 + Rust 自动重编译）
npm run tauri build      # 打包 NSIS 安装包，产物在 src-tauri/target/release/bundle/nsis/
```

应用图标源文件在 `assets/app-icon.svg`（手写矢量图）；修改后运行 `npm run tauri icon assets/app-icon.svg` 重新生成 `src-tauri/icons/` 全套图标。
托盘「未读」版本图标（`src-tauri/icons/tray-unread.png`）由 `cargo run --example gen_tray_unread`（在 `src-tauri/` 目录下运行）从 `icons/32x32.png` 合成，修改主图标后需重新生成一次。

国内网络下首次打包若卡在下载 NSIS 工具链，可设置 GitHub 镜像后重试：

```bash
set TAURI_BUNDLER_TOOLS_GITHUB_MIRROR=https://你的镜像地址
```

### 发版流程（维护者）

```bash
node scripts/bump-version.mjs 0.2.0    # 同步 Cargo.toml 与 package.json 的版本号
# 提交（gitmoji 规范）后打 tag 并推送双远端：
git tag v0.2.0
git push origin main --tags && git push gitee main --tags
```

tag 推送后 GitHub Actions 自动构建并创建 GitHub Release，随后调用
`scripts/gitee-release.mjs` 经 Gitee OpenAPI 同步创建 Gitee Release 并上传安装包与校验文件
（需要仓库 secret `GITEE_TOKEN`，本地补传时设置同名环境变量运行同一脚本即可）。

## 技术栈

Tauri 2（Rust）+ Vue 3 + TypeScript + Tailwind CSS v4；本地存储 SQLite（rusqlite）；
HTTP 客户端 reqwest；IMAP 使用本地补丁版 [imap] crate（`src-tauri/vendor/imap`，上游 2.4.1 有崩溃与 ID 命令缺陷）。

## 许可证

[MIT](LICENSE)。第三方依赖声明见 `src-tauri/vendor/imap/`（Apache-2.0 / MIT）。
