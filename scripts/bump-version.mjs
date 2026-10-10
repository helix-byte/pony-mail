#!/usr/bin/env node
/**
 * 版本号升级脚本：node scripts/bump-version.mjs <x.y.z>
 *
 * 版本号 SSOT 是 src-tauri/Cargo.toml 的 [package].version
 * （tauri.conf.json 已不写 version，打包与运行时都从 Cargo.toml 取）；package.json 由本脚本同步。
 * Cargo.lock 里本包的版本会在下次构建时自动更新，记得随提交一起入库。
 */
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const next = process.argv[2];

if (!next || !/^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/.test(next)) {
  console.error("用法: node scripts/bump-version.mjs <x.y.z>   例: node scripts/bump-version.mjs 0.2.0");
  process.exit(1);
}

// 1) Cargo.toml —— 只替换 [package] 段里的第一个 version，不碰依赖表里的版本号
const cargoPath = resolve(root, "src-tauri/Cargo.toml");
const cargo = readFileSync(cargoPath, "utf8");
const pkgIdx = cargo.indexOf("[package]");
if (pkgIdx < 0) throw new Error("src-tauri/Cargo.toml 中找不到 [package] 段");
const head = cargo.slice(0, pkgIdx);
const body = cargo.slice(pkgIdx);
const bodyNext = body.replace(/^version\s*=\s*"[^"]+"/m, `version = "${next}"`);
if (bodyNext === body) throw new Error("src-tauri/Cargo.toml 的 [package] 段里找不到 version 字段");
writeFileSync(cargoPath, head + bodyNext);

// 2) package.json
const pkgPath = resolve(root, "package.json");
const pkg = JSON.parse(readFileSync(pkgPath, "utf8"));
pkg.version = next;
writeFileSync(pkgPath, JSON.stringify(pkg, null, 2) + "\n");

console.log(`✅ 版本已更新为 ${next}：`);
console.log("   - src-tauri/Cargo.toml ([package].version)");
console.log("   - package.json");
console.log("");
console.log("接下来：");
console.log("   1. 下次构建后 Cargo.lock 会自动带上新版本，随提交一起入库");
console.log(`   2. git add -A && git commit -m "🔖 release(版本): 发布 v${next}"`);
console.log(`   3. git tag v${next}`);
console.log(`   4. git push origin main --tags && git push gitee main --tags`);
