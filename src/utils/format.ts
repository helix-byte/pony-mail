/** 时间与文本格式化工具。 */

export function formatDate(ts: number): string {
  if (!ts) return "—";
  const d = new Date(ts * 1000);
  const now = new Date();
  if (d.toDateString() === now.toDateString()) {
    return d.toLocaleTimeString("zh-CN", { hour: "2-digit", minute: "2-digit" });
  }
  if (d.getFullYear() === now.getFullYear()) {
    return d.toLocaleDateString("zh-CN", { month: "2-digit", day: "2-digit" });
  }
  return d.toLocaleDateString("zh-CN", {
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
  });
}

export function formatDateTime(ts: number): string {
  if (!ts) return "时间未知";
  return new Date(ts * 1000).toLocaleString("zh-CN", {
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  });
}

export function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

/** 取头像文字：优先姓名首字，否则邮箱首字母。 */
export function avatarText(name: string, email: string): string {
  const trimmed = name.trim();
  if (trimmed) return trimmed.slice(0, 1).toUpperCase();
  const e = email.trim();
  return e ? e.slice(0, 1).toUpperCase() : "?";
}

/** 标签可选色板。新建标签时按序轮转取色，保证互不撞色。 */
export const TAG_PALETTE: string[] = [
  "#6366f1",
  "#8b5cf6",
  "#ec4899",
  "#f97316",
  "#14b8a6",
  "#0ea5e9",
  "#84cc16",
  "#f43f5e",
  "#eab308",
  "#64748b",
];

/**
 * 标签组可选色板。
 *
 * 刻意与 TAG_PALETTE 错开色相重心（偏向紫/青/玫红），
 * 让侧栏里「组」和「标签」在视觉上能一眼分辨层级。
 */
export const GROUP_PALETTE: string[] = [
  "#8b5cf6",
  "#a855f7",
  "#d946ef",
  "#0891b2",
  "#0d9488",
  "#7c3aed",
  "#c026d3",
  "#0e7490",
];

/**
 * 标签「选中指示点」的视觉方案：彩色圆点 + 同色辉光。
 *
 * 未选中时圆点以低不透明度示人——保留标签本色但不抢注意力；
 * 选中时恢复全色，并叠一圈内柔光与外扩光晕，形成"发光"观感。
 *
 * 颜色来自数据库（用户可存任意 hex），无法预先编进 Tailwind 类，
 * 因此这里返回内联 style 对象。
 */
export function tagDotStyle(color: string, selected: boolean): Record<string, string> {
  if (!selected) return { background: color, opacity: "0.35" };
  return {
    background: color,
    boxShadow: `0 0 0 2.5px ${withAlpha(color, 0.22)}, 0 0 8px 1.5px ${withAlpha(color, 0.85)}`,
  };
}

/** 把 #rgb / #rrggbb 转成 rgba(...)，非法输入原样返回（不抛错）。 */
function withAlpha(color: string, alpha: number): string {
  const matched = /^#?([0-9a-f]{3}|[0-9a-f]{6})$/i.exec(color.trim());
  if (!matched) return color;
  let hex = matched[1];
  if (hex.length === 3) hex = hex.split("").map((c) => c + c).join("");
  const n = Number.parseInt(hex, 16);
  return `rgba(${(n >> 16) & 255}, ${(n >> 8) & 255}, ${n & 255}, ${alpha})`;
}

/** 依据字符串生成稳定的头像底色（保证同一发件人颜色一致）。 */
export function avatarColor(seed: string): string {
  const palette = [
    "#6366f1",
    "#8b5cf6",
    "#ec4899",
    "#f97316",
    "#14b8a6",
    "#0ea5e9",
    "#84cc16",
    "#f43f5e",
  ];
  let hash = 0;
  for (let i = 0; i < seed.length; i += 1) {
    hash = (hash * 31 + seed.charCodeAt(i)) % 100000;
  }
  return palette[hash % palette.length];
}
