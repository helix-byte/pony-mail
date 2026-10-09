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
