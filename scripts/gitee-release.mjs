#!/usr/bin/env node
/**
 * 把某个 tag 的发布同步到 Gitee Release（幂等，可重复执行、CI 与本地两用）。
 *
 * 用法: node scripts/gitee-release.mjs <tag>     例: node scripts/gitee-release.mjs v0.1.0
 *
 * 环境变量:
 *   GITEE_TOKEN   必填，Gitee 私人令牌（projects 范围）；未配置时打印警告并跳过（退出码 0）
 *   GITEE_OWNER   必填（缺省回退 GITHUB_OWNER）
 *   GITEE_REPO    可选，默认 pony-mail
 *   GITHUB_OWNER  可选，默认取 GITEE_OWNER（用于读取 GitHub Release 的名称与说明，保证两边文案一致）
 *   GITHUB_REPO   可选，默认 pony-mail
 *   GITHUB_TOKEN  可选，仅用于提高 GitHub API 限额（读公开 Release 本身不需要）
 *
 * 资产来源：src-tauri/target/release/bundle/nsis/*.exe。
 * 上传前把文件名中的空格替换为 "-"（"Pony Mail_0.1.0_x64-setup.exe" → "Pony-Mail_0.1.0_x64-setup.exe"），
 * 并生成同名 .sha256 校验文件一并上传。
 */
import { createHash } from "node:crypto";
import { existsSync, readFileSync, readdirSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");

const tag = process.argv[2];
if (!tag || !/^v?\d+\.\d+\.\d+/.test(tag)) {
  console.error("用法: node scripts/gitee-release.mjs <tag>（如 v0.1.0）");
  process.exit(1);
}

const token = process.env.GITEE_TOKEN;
const owner = process.env.GITEE_OWNER || process.env.GITHUB_OWNER;
const repo = process.env.GITEE_REPO || "pony-mail";
const ghOwner = process.env.GITHUB_OWNER || owner;
const ghRepo = process.env.GITHUB_REPO || "pony-mail";

if (!token) {
  console.warn("⚠️ 未配置 GITEE_TOKEN，跳过 Gitee 同步（GitHub Release 不受影响）");
  process.exit(0);
}
if (!owner) {
  console.error("缺少环境变量：需要 GITEE_OWNER（或 GITHUB_OWNER）");
  process.exit(1);
}

const GITEE_API = "https://gitee.com/api/v5";
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/** 带重试的 fetch：网络错误与 5xx 最多重试 3 次。 */
async function request(url, options = {}, tries = 3) {
  let lastErr;
  for (let i = 1; i <= tries; i++) {
    try {
      const res = await fetch(url, options);
      if (res.status >= 500 && i < tries) {
        lastErr = new Error(`HTTP ${res.status}`);
        await sleep(1500 * i);
        continue;
      }
      return res;
    } catch (e) {
      lastErr = e;
      if (i < tries) await sleep(1500 * i);
    }
  }
  throw lastErr;
}

/** 调用 Gitee API，失败时抛出带响应内容的错误。 */
async function giteeJson(path, options) {
  const url = `${GITEE_API}${path}${path.includes("?") ? "&" : "?"}access_token=${encodeURIComponent(token)}`;
  const res = await request(url, options);
  const text = await res.text();
  let data = null;
  try {
    data = JSON.parse(text);
  } catch {
    /* 保留 text 供报错 */
  }
  if (!res.ok) {
    const msg = data?.message || data?.error || text.slice(0, 300);
    throw new Error(`Gitee API ${res.status}: ${msg}`);
  }
  return data;
}

// ---- 1. 采集本地资产 ----
const assetDir = resolve(root, "src-tauri/target/release/bundle/nsis");
if (!existsSync(assetDir)) {
  console.error(`找不到资产目录：${assetDir}\n请先 npm run tauri build`);
  process.exit(1);
}
const rawAssets = readdirSync(assetDir).filter((f) => f.endsWith(".exe"));
if (rawAssets.length === 0) {
  console.error(`${assetDir} 里没有 .exe 安装包，请先 npm run tauri build`);
  process.exit(1);
}
const assets = rawAssets.map((name) => {
  const buf = readFileSync(resolve(assetDir, name));
  return {
    name: name.replace(/\s+/g, "-"),
    buf,
    sha: createHash("sha256").update(buf).digest("hex"),
  };
});
console.log(`📦 待上传资产（${assets.length} 个）：`);
for (const a of assets) console.log(`   - ${a.name}  sha256=${a.sha.slice(0, 16)}…`);

// ---- 2. 读取 GitHub Release 的 name/body（保证两边文案一致）----
let releaseName = `Pony Mail ${tag}`;
let releaseBody = "";
try {
  const headers = {
    "User-Agent": "pony-mail-release-script",
    Accept: "application/vnd.github+json",
  };
  if (process.env.GITHUB_TOKEN) headers.Authorization = `Bearer ${process.env.GITHUB_TOKEN}`;
  const res = await request(`https://api.github.com/repos/${ghOwner}/${ghRepo}/releases/tags/${tag}`, { headers });
  if (res.ok) {
    const gh = await res.json();
    releaseName = gh.name || releaseName;
    releaseBody = gh.body || "";
    console.log(`ℹ️ 已读取 GitHub Release 文案（${ghOwner}/${ghRepo} @ ${tag}）`);
  } else {
    console.warn(`⚠️ 未找到 GitHub Release（HTTP ${res.status}），将使用默认文案`);
  }
} catch (e) {
  console.warn(`⚠️ 读取 GitHub Release 失败（${e.message}），将使用默认文案`);
}

// ---- 3. 查找或创建 Gitee Release ----
async function findRelease() {
  for (let page = 1; page <= 10; page++) {
    const list = await giteeJson(`/repos/${owner}/${repo}/releases?page=${page}&per_page=100`);
    if (!Array.isArray(list) || list.length === 0) return null;
    const hit = list.find((r) => r.tag_name === tag);
    if (hit) return hit;
  }
  return null;
}

let release = await findRelease();
if (release) {
  console.log(`ℹ️ Gitee 已存在 ${tag} 的 Release（id=${release.id}），将补传缺失资产`);
} else {
  console.log(`🚀 创建 Gitee Release：${tag}`);
  const form = new URLSearchParams({
    tag_name: tag,
    name: releaseName,
    body: releaseBody || "详见 GitHub Release 说明。",
    target_commitish: process.env.GITEE_TARGET_COMMITISH || "main",
    prerelease: "false",
  });
  release = await giteeJson(`/repos/${owner}/${repo}/releases`, {
    method: "POST",
    headers: { "Content-Type": "application/x-www-form-urlencoded" },
    body: form.toString(),
  });
  if (!release?.id) {
    throw new Error(
      `创建 Gitee Release 失败：${JSON.stringify(release).slice(0, 300)}\n` +
        `提示：确认 tag 已推送到 Gitee（git push gitee ${tag}）`,
    );
  }
  console.log(`✅ 已创建（id=${release.id}）`);
}

// ---- 4. 上传缺失的附件（幂等：同名跳过）----
const existing = await giteeJson(`/repos/${owner}/${repo}/releases/${release.id}/attach_files?page=1&per_page=100`);
const existingNames = new Set(
  (Array.isArray(existing) ? existing : []).map((a) => a.name || a.title || a.filename || ""),
);

async function upload(name, buf) {
  if (existingNames.has(name)) {
    console.log(`   ⏭️ 已存在，跳过：${name}`);
    return;
  }
  const fd = new FormData();
  fd.append("file", new Blob([buf], { type: "application/octet-stream" }), name);
  const res = await request(
    `${GITEE_API}/repos/${owner}/${repo}/releases/${release.id}/attach_files?access_token=${encodeURIComponent(token)}`,
    { method: "POST", body: fd },
  );
  const text = await res.text();
  if (!res.ok) throw new Error(`上传 ${name} 失败（HTTP ${res.status}）：${text.slice(0, 300)}`);
  console.log(`   ✅ 已上传：${name}`);
}

for (const a of assets) {
  await upload(a.name, a.buf);
  await upload(`${a.name}.sha256`, Buffer.from(`${a.sha}  ${a.name}\n`, "utf8"));
}

console.log("");
console.log(`🎉 完成。Gitee Release：https://gitee.com/${owner}/${repo}/releases/tag/${tag}`);
