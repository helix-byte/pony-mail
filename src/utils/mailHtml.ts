/**
 * 邮件正文 HTML 的渲染辅助：构造 sandbox iframe 的 srcdoc。
 *
 * 深色模式策略（「强制深色」）：系统深色偏好下，正文一律以深色呈现，
 * 且做针对性适配而非盲目反转，分两条路径：
 *
 * 1. 邮件自带深色适配（含 `@media (prefers-color-scheme: dark)`）：
 *    直接用作者设计的深色版，不做任何叠加处理；
 * 2. 邮件未适配（多数通知/事务类邮件）：注入强制深色转换——
 *    `invert(1) hue-rotate(180deg)` 保色相明暗反转：白底转暗底、
 *    深色文字转亮色、蓝色链接转为适配暗底的亮蓝，图片二次反转恢复原样。
 *
 * 另外 `color-scheme: only light`（html 内联 + meta + :root）用于让
 * Chromium Auto-Dark（强制深色特性）跳过本文档，避免与本文件的转换叠加
 * 造成双重反转；它不影响 prefers-color-scheme 媒体查询的正常匹配。
 */

/** 找出所有 `@media` 的位置（CSS 关键字大小写不敏感）。 */
const AT_MEDIA = /@media/gi;

/**
 * 邮件是否自带深色适配：存在条件含 `prefers-color-scheme: dark` 的
 * `@media` 块。取 `@media` 到第一个 `{` 之间的条件串（去注释、去空白、
 * 转小写）判断，覆盖大小写与无空格写法。
 */
export function hasDarkMediaQuery(html: string): boolean {
  AT_MEDIA.lastIndex = 0;
  for (let m = AT_MEDIA.exec(html); m; m = AT_MEDIA.exec(html)) {
    const brace = html.indexOf("{", m.index);
    if (brace === -1) return false; // 其后没有块，无需再找
    const cond = html
      .slice(m.index + "@media".length, brace)
      .replace(/\/\*[\s\S]*?\*\//g, "")
      .replace(/\s+/g, "")
      .toLowerCase();
    if (
      cond.includes("prefers-color-scheme:dark") ||
      cond.includes("prefers-color-scheme:onlydark")
    ) {
      return true;
    }
  }
  return false;
}

/**
 * 未适配邮件在深色偏好下的强制深色转换。
 *
 * - `html` 上保色相反转；显式声明反转前的底色为白，保证反转后底色稳定为暗；
 * - `img/picture/video` 在已反转的坐标系内再次反转（invert 与 hue-rotate
 *   均自逆），两次转换精确抵消，照片/截图恢复原样；
 * - 内联 `<svg>` 图标不保护：多为深色描边，反转后变亮色反而在暗底可见。
 */
const FORCE_DARK_CSS = `
@media (prefers-color-scheme: dark){
html{background:#fff;filter:invert(1) hue-rotate(180deg)}
img,picture,video{filter:invert(1) hue-rotate(180deg)}
}`;

/**
 * 把清洗后的邮件正文包装为 iframe 的 srcdoc 文档。
 *
 * 注入样式只兜底「邮件自身未声明」的部分，保持克制：
 * - 不设置链接颜色（交还邮件样式与浏览器默认，避免覆盖作者配色）；
 * - 表格只限制最大宽度，不改 `border-collapse`（邮件常依赖默认的
 *   separate 布局模型与 cellspacing 控制间距）。
 */
export function buildMailSrcdoc(bodyHtml: string): string {
  const forceDark = hasDarkMediaQuery(bodyHtml) ? "" : FORCE_DARK_CSS;
  return `<!doctype html><html style="color-scheme:only light"><head><meta charset="utf-8"><meta name="color-scheme" content="only light"><base target="_blank"><style>
:root{color-scheme:only light}
html,body{margin:0;padding:0}
body{font-family:-apple-system,"Segoe UI","Microsoft YaHei",sans-serif;font-size:14px;line-height:1.72;color:#18181b;padding:18px 22px;word-break:break-word;background:#fff}${forceDark}
img{max-width:100%;height:auto}
table{max-width:100%}
pre{white-space:pre-wrap;font-family:inherit}
blockquote{border-left:3px solid #e4e4e7;margin:8px 0;padding-left:12px;color:#52525b}
</style></head><body>${bodyHtml}</body></html>`;
}
