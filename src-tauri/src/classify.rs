//! 智能整理的纯逻辑层：prompt 构造、模型输出解析容错链、新标签名校验与配色。
//!
//! 与 IO 完全解耦（不碰数据库、不发请求），便于用单元测试盯住容错行为。
//!
//! 为什么需要容错链：小模型即便在 `format:"json"` 约束下产出合法 JSON，
//! **值也常常填错**——实测出现过只读维度被臆造值（候选只有「促销」却答「新登录」）、
//! null 位置填 JSON 布尔、整个对象 schema 跑偏（`{"name":...,"tags":[...]}`）。
//! 因此「精确 → 双向包含取最近长度 → 未知值降级 → 低置信回退」不是冗余，
//! 而是主路径的一部分；整体跑偏时才判失败（不写标记，下次重试）。

use serde_json::Value;

use crate::ai::{format_time, reduce_noise, truncate};

// ---------------------------------------------------------------------
// 常量
// ---------------------------------------------------------------------

/// 低置信回退门槛：整体置信度低于它时全部维度不指派
/// （但这封仍算「已整理」，标记照写，避免反复重试同一封）。
pub const CONFIDENCE_FLOOR: f64 = 0.4;
/// 模型没给 confidence 时的默认值（视为基本可信）。
pub const DEFAULT_CONFIDENCE: f64 = 0.6;
/// 组内候选注入 prompt 的上限，超出省略（防标签多时 prompt 爆长）。
pub const CANDIDATE_HINT_LIMIT: usize = 30;
/// 正文节选长度上限（字符）。
pub const BODY_EXCERPT_LIMIT: usize = 1200;
/// snippet 长度上限（正文未加载时的降级输入）。
pub const SNIPPET_LIMIT: usize = 400;
/// 发件人快速通道的历史样本下限。
pub const SENDER_SKIP_MIN_SAMPLES: i64 = 3;

/// 新标签名长度范围（字符数）。
pub const NEW_NAME_MIN_CHARS: usize = 2;
pub const NEW_NAME_MAX_CHARS: usize = 16;
/// 新标签名禁用字符（对照 Hony 已验证的常量）。
const NEW_NAME_FORBIDDEN: [char; 8] = ['@', '\n', '\r', ':', '：', '/', '\\', '|'];

/// 标签配色板——必须与前端 `src/utils/format.ts` 的 `TAG_PALETTE` 逐字一致，
/// 保证 AI 创建的标签与用户手建标签的配色语言统一。
pub const TAG_PALETTE: [&str; 10] = [
    "#6366f1", "#8b5cf6", "#ec4899", "#f97316", "#14b8a6", "#0ea5e9", "#84cc16", "#f43f5e",
    "#eab308", "#64748b",
];

// ---------------------------------------------------------------------
// 数据结构
// ---------------------------------------------------------------------

/// 维度组的 AI 模式（对应 `tag_groups.ai_mode` 的 `"select"` / `"create"`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupAiMode {
    /// 只能从组内现有标签中选一个或 null，不得创建。
    Select,
    /// 优先组内选择；选不上但邮件明显是品牌时，创建新标签并自动入组。
    Create,
}

impl GroupAiMode {
    /// 从数据库存值解析；`"off"` 或未知值返回 `None`（该组不参与整理）。
    pub fn from_db(raw: &str) -> Option<Self> {
        match raw {
            "select" => Some(Self::Select),
            "create" => Some(Self::Create),
            _ => None,
        }
    }
}

/// 参与整理的一个维度：组 + 模式 + 组内候选标签。
#[derive(Debug, Clone)]
pub struct DimensionGroup {
    pub id: i64,
    pub name: String,
    pub mode: GroupAiMode,
    /// 组内现有标签 `(id, 名字)`，按组内展示顺序排好。
    pub candidates: Vec<(i64, String)>,
}

/// 分类输入：一封邮件的可判据材料。
///
/// 正文**不预取**：只有已缓存（`body_loaded = 1`）时才带 `body_text`，
/// 否则退化为「主题 + 发件人 + snippet」，让整理能覆盖从未打开过的邮件。
#[derive(Debug, Clone, Default)]
pub struct ClassifyInput {
    pub from_name: String,
    pub from_addr: String,
    pub subject: String,
    pub date_ts: i64,
    pub snippet: String,
    pub body_text: Option<String>,
}

/// 单个维度的指派结果。
#[derive(Debug, Clone, PartialEq)]
pub enum Pick {
    /// 命中组内已有标签。
    Existing(i64),
    /// `create` 模式下诞生的新标签名（由写回层解析或创建）。
    New(String),
}

/// 解析结果：`picks` 与传入的 `groups` 一一对齐（`None` = 该维度不指派）。
#[derive(Debug, Clone)]
pub struct ParsedClassify {
    pub picks: Vec<Option<Pick>>,
    pub confidence: f64,
}

// ---------------------------------------------------------------------
// prompt 构造
// ---------------------------------------------------------------------

/// 构造分类系统提示词：注入维度组、候选与输出格式示例。
pub fn build_system_prompt(groups: &[DimensionGroup]) -> String {
    let dims = groups
        .iter()
        .map(format_dimension)
        .collect::<Vec<_>>()
        .join("\n");
    let keys = groups
        .iter()
        .map(|g| g.name.as_str())
        .collect::<Vec<_>>()
        .join("、");
    let example = build_example(groups);

    format!(
        "你是邮件整理助手，阅读邮件后为它指派以下 {n} 个维度的标签。\
         只输出一个 JSON 对象，不要输出任何其他内容（不要 markdown 代码块、不要解释）。\n\
         \n\
         维度与可选值：\n\
         {dims}\n\
         \n\
         输出格式（键为维度名，必须逐字使用；值为标签名或 null）：\n\
         {example}\n\
         \n\
         规则：\n\
         - 必须输出且只输出「{keys}」这些键，以及 confidence，一个不多、一个不少。\n\
         - 不允许创建新值的维度：只能从列表中原样选择或 null，不要自造新标签。\n\
         - 允许创建的维度：优先复用已有标签；只有邮件明显来自某个产品/服务/机构品牌时，\
         才创建 2-16 个字的新名称，不含 @、斜杠、冒号、换行等符号；同一品牌始终使用同一名称。\n\
         - 无法判断或不确定时值必须为 null，不要猜测，不要用 false、空字符串或「无」之类的占位。\n\
         - confidence 是 0 到 1 的小数，表示整体判断的把握。\n\
         - 全部使用简体中文；品牌名保留常用原名。",
        n = groups.len(),
        dims = dims,
        keys = keys,
        example = example,
    )
}

fn format_dimension(g: &DimensionGroup) -> String {
    let mut names: Vec<&str> = g.candidates.iter().map(|(_, n)| n.as_str()).collect();
    let total = names.len();
    names.truncate(CANDIDATE_HINT_LIMIT);
    let omitted = if total > names.len() { "（其余省略）" } else { "" };

    match g.mode {
        GroupAiMode::Select if names.is_empty() => {
            format!("- {}：该维度暂时没有可选标签，一律输出 null。", g.name)
        }
        GroupAiMode::Select => {
            let listed = names
                .iter()
                .map(|n| format!("  - {n}"))
                .collect::<Vec<_>>()
                .join("\n");
            format!(
                "- {}：必须从下面列表中原样选一个，都不匹配时输出 null：\n{listed}{omitted}",
                g.name
            )
        }
        GroupAiMode::Create if names.is_empty() => {
            format!(
                "- {}：若邮件明显来自某个产品/服务/机构品牌（如 Steam、Microsoft），\
                 可用其通用短名（2-16 个字，不含 @ 斜杠冒号换行等符号）作为新标签输出；\
                 个人邮件、无法判断时输出 null。",
                g.name
            )
        }
        GroupAiMode::Create => {
            format!(
                "- {}：优先复用已有标签：{}{omitted}；若邮件明显来自某个产品/服务/机构品牌，\
                 可用其通用短名（2-16 个字，不含 @ 斜杠冒号换行等符号）作为新标签输出；\
                 个人邮件、无法判断时输出 null。",
                g.name,
                names.join("、"),
            )
        }
    }
}

/// 构造输出格式示例：第一个维度展示真实取值形态（优先用真实候选名），其余 null。
fn build_example(groups: &[DimensionGroup]) -> String {
    let sample_value = groups
        .iter()
        .find_map(|g| g.candidates.first())
        .map(|(_, n)| n.clone())
        .unwrap_or_else(|| "Steam".to_string());
    let mut parts: Vec<String> = Vec::with_capacity(groups.len() + 1);
    for (i, g) in groups.iter().enumerate() {
        if i == 0 {
            parts.push(format!("\"{}\":\"{sample_value}\"", g.name));
        } else {
            parts.push(format!("\"{}\":null", g.name));
        }
    }
    parts.push("\"confidence\":0.9".to_string());
    format!("{{{}}}", parts.join(","))
}

/// 构造分类用户消息。
pub fn build_user_prompt(input: &ClassifyInput, hints: &[String], now: &str) -> String {
    let mut s = String::from("请为以下邮件指派标签。\n\n");
    s.push_str(&format!("当前时间：{now}\n"));
    let from = if input.from_name.trim().is_empty() {
        input.from_addr.trim().to_string()
    } else {
        format!("{} <{}>", input.from_name.trim(), input.from_addr.trim())
    };
    s.push_str(&format!("发件人：{from}\n"));
    s.push_str(&format!("时间：{}\n", format_time(input.date_ts)));
    s.push_str(&format!("主题：{}\n", input.subject.trim()));

    // 正文与摘要二选一：有已缓存正文就用降噪后的节选，否则退化为 snippet。
    let excerpt = input
        .body_text
        .as_deref()
        .map(|b| truncate(&reduce_noise(b), BODY_EXCERPT_LIMIT))
        .unwrap_or_default();
    if !excerpt.trim().is_empty() {
        s.push_str(&format!("正文节选：\n{excerpt}\n"));
    } else {
        let snip = truncate(input.snippet.trim(), SNIPPET_LIMIT);
        if !snip.is_empty() {
            s.push_str(&format!("摘要：{snip}\n"));
        }
    }

    for h in hints {
        s.push_str(&format!("提示：{h}\n"));
    }
    s.trim_end().to_string()
}

// ---------------------------------------------------------------------
// 模型输出解析（容错链）
// ---------------------------------------------------------------------

/// 解析模型输出。整体失败（JSON 跑偏 / 键全不匹配）返回 `None`，
/// 由调用方计入失败且**不写已整理标记**（下次自动重试）。
pub fn parse_classify(raw: &str, groups: &[DimensionGroup]) -> Option<ParsedClassify> {
    let json = extract_json_object(raw)?;
    let value: Value = serde_json::from_str(json).ok()?;
    let obj = value.as_object()?;

    // ---- 键名匹配：每个组找它对应的键 ----
    let mut matched: Vec<Option<&str>> = Vec::with_capacity(groups.len());
    let mut hits = 0usize;
    for g in groups {
        let key = match_key(obj.keys().map(String::as_str), &g.name);
        if key.is_some() {
            hits += 1;
        }
        matched.push(key);
    }
    // 整个对象 schema 跑偏（预期键一个都对不上）→ 判为整封失败。
    if hits == 0 {
        return None;
    }

    let confidence = parse_confidence(obj);
    let picks = groups
        .iter()
        .zip(&matched)
        .map(|(g, key)| {
            if confidence < CONFIDENCE_FLOOR {
                // 低置信：全部不指派（保守），但仍算「已整理」避免无限重试。
                return None;
            }
            key.and_then(|k| obj.get(k)).and_then(|v| pick_from_value(v, g))
        })
        .collect();

    Some(ParsedClassify { picks, confidence })
}

/// 提取第一个 JSON 对象：剥 ```` ```json ```` / ```` ``` ```` 围栏，取首 `{` 到末 `}`。
///
/// `format:"json"` 下通常用不到，作为兜底保留。
fn extract_json_object(raw: &str) -> Option<&str> {
    let t = raw.trim();
    let t = t
        .strip_prefix("```json")
        .or_else(|| t.strip_prefix("```"))
        .unwrap_or(t);
    let t = t.strip_suffix("```").unwrap_or(t);
    let start = t.find('{')?;
    let end = t.rfind('}')?;
    if end < start {
        return None;
    }
    Some(&t[start..=end])
}

/// 键名匹配：精确 → 去尾部冒号 → 双向包含取长度最近（键 ≥2 字）。
fn match_key<'a, I: Iterator<Item = &'a str>>(keys: I, group_name: &str) -> Option<&'a str> {
    let keys: Vec<&'a str> = keys.collect();
    if let Some(k) = keys.iter().find(|k| **k == group_name) {
        return Some(k);
    }
    let trimmed = |k: &str| k.trim().trim_end_matches([':', '：']).trim().to_string();
    if let Some(k) = keys.iter().find(|k| trimmed(k) == group_name) {
        return Some(k);
    }
    let gn_len = group_name.chars().count();
    let mut best: Option<(&'a str, usize)> = None;
    for k in keys {
        let k_len = k.chars().count();
        if k_len < 2 {
            continue;
        }
        if k.contains(group_name) || group_name.contains(k) {
            let diff = k_len.abs_diff(gn_len);
            if best.map_or(true, |(_, d)| diff < d) {
                best = Some((k, diff));
            }
        }
    }
    best.map(|(k, _)| k)
}

/// 单个键值的指派判定。
fn pick_from_value(value: &Value, group: &DimensionGroup) -> Option<Pick> {
    let s = value.as_str()?; // 非字符串（布尔 / 数字 / 数组 / 对象）一律不指派
    let cleaned = clean_value(s)?;
    let cleaned = strip_group_prefix(&cleaned, &group.name);

    if let Some((id, _)) = match_name(&cleaned, &group.candidates) {
        return Some(Pick::Existing(id));
    }
    if group.mode == GroupAiMode::Create && valid_new_label_name(&cleaned) {
        return Some(Pick::New(cleaned));
    }
    None
}

/// 值清洗：去首尾空白与包裹引号；空值 / 占位词（「无 / null / 不确定」等）→ `None`。
fn clean_value(raw: &str) -> Option<String> {
    let mut s = raw.trim();
    for (a, b) in [('「', '」'), ('“', '”'), ('"', '"'), ('\'', '\''), ('《', '》')] {
        if s.starts_with(a) && s.ends_with(b) && s.chars().count() >= 2 {
            s = s[a.len_utf8()..s.len() - b.len_utf8()].trim();
        }
    }
    if s.is_empty() {
        return None;
    }
    let lower = s.to_ascii_lowercase();
    if matches!(lower.as_str(), "null" | "none" | "nil" | "nan" | "-")
        || matches!(s, "无" | "没有" | "不确定" | "未知" | "不适用" | "空")
    {
        return None;
    }
    Some(s.to_string())
}

/// 值以「组名:」开头时剥掉前缀（模型偶尔把键名也重复进值里）。
fn strip_group_prefix(value: &str, group_name: &str) -> String {
    if let Some(rest) = value.strip_prefix(group_name) {
        for sep in [':', '：'] {
            if let Some(r) = rest.strip_prefix(sep) {
                let r = r.trim();
                if !r.is_empty() {
                    return r.to_string();
                }
            }
        }
    }
    value.trim().to_string()
}

/// 名称匹配链：精确 → ASCII 大小写不敏感 → 双向包含取长度最近。
///
/// **单字符不参与包含匹配**（有意为之）：否则「不重要」包含「重要」这类
/// 二值维度会造成系统性误配（Hony 有同类测试盯住）。
pub fn match_name<'a>(raw: &str, candidates: &'a [(i64, String)]) -> Option<(i64, &'a str)> {
    let needle = raw.trim();
    let needle_len = needle.chars().count();
    if needle.is_empty() {
        return None;
    }
    // 1) 精确
    if let Some((id, name)) = candidates.iter().find(|(_, n)| n == needle) {
        return Some((*id, name.as_str()));
    }
    // 2) ASCII 大小写不敏感精确（防 steam / Steam 这类重复）
    let lower = needle.to_ascii_lowercase();
    if let Some((id, name)) = candidates
        .iter()
        .find(|(_, n)| n.to_ascii_lowercase() == lower)
    {
        return Some((*id, name.as_str()));
    }
    // 3) 双向包含取长度最近；单字符（任一方）不参与
    if needle_len < 2 {
        return None;
    }
    let mut best: Option<(i64, &'a str, usize)> = None;
    for (id, name) in candidates {
        let name_len = name.chars().count();
        if name_len < 2 {
            continue;
        }
        let name_lower = name.to_ascii_lowercase();
        if name_lower.contains(&lower) || lower.contains(&name_lower) {
            let diff = name_len.abs_diff(needle_len);
            if best.map_or(true, |(_, _, d)| diff < d) {
                best = Some((*id, name.as_str(), diff));
            }
        }
    }
    best.map(|(id, name, _)| (id, name))
}

fn parse_confidence(obj: &serde_json::Map<String, Value>) -> f64 {
    match obj.get("confidence") {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(DEFAULT_CONFIDENCE),
        Some(Value::String(s)) => s.trim().parse::<f64>().unwrap_or(DEFAULT_CONFIDENCE),
        _ => DEFAULT_CONFIDENCE,
    }
}

// ---------------------------------------------------------------------
// 新标签校验、配色与发件人统计
// ---------------------------------------------------------------------

/// 新标签名校验：2~16 字，且不含 `@` / 换行 / 冒号 / 斜杠等符号。
pub fn valid_new_label_name(name: &str) -> bool {
    let t = name.trim();
    let len = t.chars().count();
    (NEW_NAME_MIN_CHARS..=NEW_NAME_MAX_CHARS).contains(&len)
        && !t.chars().any(|c| NEW_NAME_FORBIDDEN.contains(&c))
}

/// 按名字确定性选色（FNV-1a）。
///
/// 不用 `std::collections::hash_map::DefaultHasher`——它不保证跨版本稳定，
/// 同名的 AI 标签可能在重启/升级后换色。色板与前端 `format.ts` 逐字一致。
pub fn palette_color(name: &str) -> &'static str {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325; // FNV-1a offset basis
    for b in name.as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    TAG_PALETTE[(hash % TAG_PALETTE.len() as u64) as usize]
}

/// 发件人快速通道的共识判定。
///
/// 条件（与 Hony 一致）：历史样本数 ≥ `min_samples`，且每个启用组要么
/// 「无历史」（不参与），要么「唯一标签且覆盖全部样本」；至少一组有共识才值得跳过。
/// 输出与各组对齐：`Some(tag_id)` 为该组共识，`None` 为该组无历史。
pub fn sender_consensus(
    stats_by_group: &[Vec<(i64, String, i64)>],
    sample_count: i64,
    min_samples: i64,
) -> Option<Vec<Option<i64>>> {
    if sample_count < min_samples {
        return None;
    }
    let mut out = Vec::with_capacity(stats_by_group.len());
    let mut any = false;
    for stats in stats_by_group {
        match stats.as_slice() {
            // 该组无历史：不参与共识（不阻断其他组）。
            [] => out.push(None),
            [(id, _, n)] if *n == sample_count => {
                out.push(Some(*id));
                any = true;
            }
            // 有多值或计数不全 → 破了「全部一致」。
            _ => return None,
        }
    }
    if any {
        Some(out)
    } else {
        None
    }
}

/// 组装发给模型的发件人历史提示（每组至多一条 top1）。
pub fn sender_hints(
    stats_by_group: &[Vec<(i64, String, i64)>],
    group_names: &[String],
    sample_count: i64,
) -> Vec<String> {
    let mut hints = Vec::new();
    for (stats, name) in stats_by_group.iter().zip(group_names) {
        if let Some((_, top_name, n)) = stats.first() {
            hints.push(format!(
                "该发件人此前 {sample_count} 封已整理邮件中，{n} 封的「{name}」为「{top_name}」"
            ));
        }
    }
    hints
}

// ---------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn groups() -> Vec<DimensionGroup> {
        vec![
            DimensionGroup {
                id: 1,
                name: "发件方".to_string(),
                mode: GroupAiMode::Create,
                candidates: vec![(10, "NVIDA".to_string()), (11, "Microsoft".to_string())],
            },
            DimensionGroup {
                id: 2,
                name: "内容".to_string(),
                mode: GroupAiMode::Select,
                candidates: vec![(20, "促销".to_string())],
            },
        ]
    }

    // ---- 解析容错链 ----

    #[test]
    fn parses_wellformed_response() {
        let raw = r#"{"发件方":"NVIDA","内容":"促销","confidence":0.9}"#;
        let parsed = parse_classify(raw, &groups()).expect("应解析成功");
        assert_eq!(parsed.picks[0], Some(Pick::Existing(10)));
        assert_eq!(parsed.picks[1], Some(Pick::Existing(20)));
        assert!((parsed.confidence - 0.9).abs() < 1e-6);
    }

    #[test]
    fn strips_json_fence() {
        let raw = "```json\n{\"发件方\":null,\"内容\":\"促销\",\"confidence\":0.8}\n```";
        let parsed = parse_classify(raw, &groups()).expect("剥围栏后应解析成功");
        assert_eq!(parsed.picks[0], None);
        assert_eq!(parsed.picks[1], Some(Pick::Existing(20)));
    }

    /// 实测样例：整对象 schema 跑偏（键全不匹配）→ 判整封失败，不写标记、下次重试。
    #[test]
    fn mis_schema_object_fails_whole() {
        let raw = r#"{"name":"microsoft","confidence":0.9,"tags":["microsoft"]}"#;
        assert!(parse_classify(raw, &groups()).is_none());
    }

    #[test]
    fn empty_object_fails() {
        assert!(parse_classify("{}", &groups()).is_none());
    }

    /// 实测样例：null 位置填了 JSON 布尔 → 该维度不指派（但不是整封失败）。
    #[test]
    fn boolean_value_not_assigned() {
        let raw = r#"{"发件方":"NVIDA","内容":false,"confidence":0.9}"#;
        let parsed = parse_classify(raw, &groups()).expect("键匹配应成功");
        assert_eq!(parsed.picks[0], Some(Pick::Existing(10)));
        assert_eq!(parsed.picks[1], None, "布尔值不得指派");
    }

    /// 实测样例：select 维度被臆造值（候选仅「促销」却答「新登录」）→ 拒绝。
    #[test]
    fn unknown_value_rejected_in_select() {
        let raw = r#"{"发件方":"NVIDA","内容":"新登录","confidence":0.9}"#;
        let parsed = parse_classify(raw, &groups()).unwrap();
        assert_eq!(parsed.picks[1], None, "select 维度不得臆造");
    }

    /// create 维度上合法的品牌新名 → Pick::New。
    #[test]
    fn new_name_in_create_mode() {
        let raw = r#"{"发件方":"NVIDIA","内容":null,"confidence":0.9}"#;
        let parsed = parse_classify(raw, &groups()).unwrap();
        assert_eq!(parsed.picks[0], Some(Pick::New("NVIDIA".to_string())));
    }

    /// 双向包含：模型把名字缩写/加长（「账单提醒」→「账单」）→ 命中已有标签。
    #[test]
    fn fuzzy_hit_via_containment() {
        let bill = vec![(30i64, "账单".to_string())];
        assert_eq!(match_name("账单提醒", &bill).map(|(id, _)| id), Some(30));
        // 反向也成立：模型输出更长、包含候选名的值。
        let promo = vec![(40i64, "促销".to_string())];
        assert_eq!(match_name("促销活动", &promo).map(|(id, _)| id), Some(40));
    }

    /// 编辑距离 1 但非子串的近似名（NVIDA vs NVIDIA）**不**命中——
    /// 双向包含匹配兜不住这类差异。create 模式下它会走「新标签」路径
    /// （合法新名），由用户改名合并；这是已知残余风险（与 Hony 一致）。
    #[test]
    fn near_miss_by_one_char_is_not_containment() {
        let candidates = vec![(10i64, "NVIDA".to_string())];
        assert_eq!(match_name("NVIDIA", &candidates), None, "非子串关系不命中");
    }

    /// 单字符不参与包含匹配：防「不重要」命中「重要」这类系统性误配。
    #[test]
    fn containment_requires_two_chars() {
        let axis = vec![(1i64, "重要".to_string()), (2i64, "不重要".to_string())];
        // 单字符 needle 不参与包含匹配。
        assert_eq!(match_name("重", &axis), None);
        // 单字符候选同样不参与。
        let tiny = vec![(3i64, "重".to_string())];
        assert_eq!(match_name("重要", &tiny), None);
        // 完整值仍精确命中。
        assert_eq!(match_name("不重要", &axis).map(|(id, _)| id), Some(2));
    }

    /// 大小写不敏感精确：steam → Steam（防同义重复）。
    #[test]
    fn case_insensitive_exact_match() {
        let candidates = vec![(5i64, "Steam".to_string())];
        assert_eq!(match_name("steam", &candidates).map(|(id, _)| id), Some(5));
    }

    #[test]
    fn low_confidence_clears_all_picks() {
        let raw = r#"{"发件方":"NVIDA","内容":"促销","confidence":0.3}"#;
        let parsed = parse_classify(raw, &groups()).expect("低置信仍算解析成功");
        assert!(parsed.picks.iter().all(Option::is_none), "低置信应全部不指派");
    }

    #[test]
    fn missing_confidence_defaults() {
        let raw = r#"{"发件方":"NVIDA","内容":null}"#;
        let parsed = parse_classify(raw, &groups()).unwrap();
        assert!((parsed.confidence - DEFAULT_CONFIDENCE).abs() < 1e-6);
    }

    #[test]
    fn placeholder_values_rejected() {
        for v in ["无", "null", "None", "不确定", "未知", "空", ""] {
            let raw = format!(r#"{{"发件方":"NVIDA","内容":"{v}","confidence":0.9}}"#);
            let parsed = parse_classify(&raw, &groups()).unwrap();
            assert_eq!(parsed.picks[1], None, "占位值「{v}」不应指派");
        }
    }

    #[test]
    fn group_prefix_in_value_stripped() {
        let raw = r#"{"发件方":"NVIDA","内容":"内容：促销","confidence":0.9}"#;
        let parsed = parse_classify(raw, &groups()).unwrap();
        assert_eq!(parsed.picks[1], Some(Pick::Existing(20)));
    }

    #[test]
    fn key_with_trailing_colon_matches() {
        let raw = r#"{"发件方：":"NVIDA","内容":"促销","confidence":0.9}"#;
        let parsed = parse_classify(raw, &groups()).unwrap();
        assert_eq!(parsed.picks[0], Some(Pick::Existing(10)));
    }

    /// 部分键命中：未命中的组视为不指派，整体仍成功。
    #[test]
    fn partial_key_match_still_succeeds() {
        let raw = r#"{"内容":"促销","confidence":0.9}"#;
        let parsed = parse_classify(raw, &groups()).expect("部分命中应成功");
        assert_eq!(parsed.picks[0], None);
        assert_eq!(parsed.picks[1], Some(Pick::Existing(20)));
    }

    // ---- 新标签校验与配色 ----

    #[test]
    fn valid_new_label_name_rules() {
        assert!(valid_new_label_name("Microsoft"));
        assert!(valid_new_label_name("NVIDIA"));
        assert!(!valid_new_label_name("a@b.com"), "含 @ 应拒绝");
        assert!(!valid_new_label_name("甲"), "单字应拒绝");
        assert!(!valid_new_label_name(&"很".repeat(17)), "超 16 字应拒绝");
        assert!(!valid_new_label_name("a/b"), "含斜杠应拒绝");
        assert!(!valid_new_label_name("a：b"), "含冒号应拒绝");
        assert!(!valid_new_label_name(" a\nb "), "含换行应拒绝");
    }

    #[test]
    fn palette_color_is_deterministic() {
        let a = palette_color("Microsoft");
        let b = palette_color("Microsoft");
        assert_eq!(a, b, "同名必须同色");
        assert!(TAG_PALETTE.contains(&palette_color("Steam")));
        assert!(TAG_PALETTE.contains(&palette_color("随意名字")));
    }

    // ---- 发件人快速通道 ----

    #[test]
    fn consensus_requires_min_samples() {
        let stats = vec![
            vec![(10i64, "Microsoft".to_string(), 2i64)],
            vec![(20i64, "促销".to_string(), 2i64)],
        ];
        assert!(sender_consensus(&stats, 2, SENDER_SKIP_MIN_SAMPLES).is_none(), "样本不足不跳过");
    }

    #[test]
    fn consensus_when_all_groups_unanimous() {
        let stats = vec![
            vec![(10i64, "Microsoft".to_string(), 5i64)],
            vec![(20i64, "促销".to_string(), 5i64)],
        ];
        let c = sender_consensus(&stats, 5, SENDER_SKIP_MIN_SAMPLES).expect("应形成共识");
        assert_eq!(c, vec![Some(10), Some(20)]);
    }

    #[test]
    fn consensus_broken_by_multiple_values() {
        let stats = vec![
            vec![(10i64, "Microsoft".to_string(), 3i64), (11i64, "Steam".to_string(), 2i64)],
            vec![(20i64, "促销".to_string(), 5i64)],
        ];
        assert!(sender_consensus(&stats, 5, SENDER_SKIP_MIN_SAMPLES).is_none(), "多值不跳过");
    }

    /// 某组无历史不阻断；但至少一组有共识才值得跳过。
    #[test]
    fn consensus_tolerates_empty_group_but_needs_any_hit() {
        let stats = vec![
            vec![(10i64, "Microsoft".to_string(), 4i64)],
            vec![],
        ];
        let c = sender_consensus(&stats, 4, SENDER_SKIP_MIN_SAMPLES).expect("应形成共识");
        assert_eq!(c, vec![Some(10), None]);

        let all_empty = vec![vec![], vec![]];
        assert!(sender_consensus(&all_empty, 5, SENDER_SKIP_MIN_SAMPLES).is_none(), "全无历史不跳过");
    }

    #[test]
    fn consensus_rejects_partial_coverage() {
        // 「唯一标签但只覆盖了一半样本」不算共识。
        let stats = vec![vec![(10i64, "Microsoft".to_string(), 2i64)]];
        assert!(sender_consensus(&stats, 5, SENDER_SKIP_MIN_SAMPLES).is_none());
    }

    #[test]
    fn hints_contain_sender_history_shape() {
        let stats = vec![vec![(10i64, "Microsoft".to_string(), 4i64)], vec![]];
        let names = vec!["发件方".to_string(), "内容".to_string()];
        let hints = sender_hints(&stats, &names, 5);
        assert_eq!(hints.len(), 1, "无历史的组不出提示");
        assert!(hints[0].contains('5') && hints[0].contains("发件方") && hints[0].contains("Microsoft"));
    }

    // ---- prompt ----

    #[test]
    fn system_prompt_contains_keys_candidates_and_rules() {
        let p = build_system_prompt(&groups());
        assert!(p.contains("发件方"), "应注入组名");
        assert!(p.contains("NVIDA"), "应注入候选");
        assert!(p.contains("confidence"), "应含 confidence 说明");
        assert!(p.contains(r#"{"发件方":"NVIDA","内容":null,"confidence":0.9}"#), "应含示例");
        assert!(p.contains("null"), "应含 null 规则");
    }

    #[test]
    fn user_prompt_prefers_body_then_snippet() {
        let mut input = ClassifyInput {
            from_name: "小明".to_string(),
            from_addr: "a@b.com".to_string(),
            subject: "主题".to_string(),
            date_ts: 0,
            snippet: "片段内容".to_string(),
            body_text: Some("正文内容".to_string()),
        };
        let p = build_user_prompt(&input, &["提示一".to_string()], "2026-10-09 22:00");
        assert!(p.contains("正文内容") && !p.contains("片段内容"), "有正文时不用 snippet");
        assert!(p.contains("提示一") && p.contains("2026-10-09 22:00"));

        input.body_text = None;
        let p = build_user_prompt(&input, &[], "2026-10-09 22:00");
        assert!(p.contains("片段内容"), "无正文时退化为 snippet");
    }
}
