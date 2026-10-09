use std::sync::LazyLock;

use regex::Regex;

/// 需要整段丢弃的危险容器标签。
///
/// 注意：Rust 的 `regex` crate **不支持反向引用**（`\1`），因此无法用
/// `<(x)>.*?</\1>` 这种成对匹配写法。这里改为「开标签 … 闭标签」的惰性匹配：
/// 由于下面的标签名都列在同一个 alternation 里，`.*?` 会一路匹配到**任意一个**
/// 闭标签，配合后置的 `DROP_ORPHAN` 逐个清理残余闭合标签，达到等效效果。
static DROP_BLOCK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?is)<(?:script|noscript|template|applet|object|embed|form|frame|frameset|iframe)\b[^>]*>.*?</(?:script|noscript|template|applet|object|embed|form|frame|frameset|iframe)\s*>",
    )
    .expect("DROP_BLOCK 正则非法")
});

/// 危险容器标签的未闭合写法（例如 `<iframe ...>` 或 `<iframe ... />`），
/// 以及上面成对匹配后残留的孤立闭合标签。
static DROP_ORPHAN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?is)</?(?:script|noscript|template|applet|object|embed|form|frame|frameset|iframe|link|meta|base)\b[^>]*>")
        .expect("DROP_ORPHAN 正则非法")
});

/// 内联事件处理器属性（onclick / onerror / onload ...）。
static DROP_EVENT_ATTR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?is)\son[a-z]+\s*=\s*(?:"[^"]*"|'[^']*'|[^\s>]+)"#)
        .expect("DROP_EVENT_ATTR 正则非法")
});

/// `javascript:` / `vbscript:` / `data:text/html` 协议链接。
///
/// 带引号与不带引号两种写法都要覆盖：`href="javascript:..."` 与 `href=javascript:...`。
static DROP_JS_URL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?is)(href|src|xlink:href|action|formaction)\s*=\s*(?:"[^"]*(?:javascript|vbscript|data:text/html)[^"]*"|'[^']*(?:javascript|vbscript|data:text/html)[^']*'|[^\s>]*(?:javascript|vbscript|data:text/html)[^\s>]*)"#,
    )
    .expect("DROP_JS_URL 正则非法")
});

/// 清洗邮件 HTML，去掉可执行内容。
///
/// 前端渲染时还会再套一层 `sandbox` iframe，这里是纵深防御的第一层：
/// 即使某条规则漏掉，沙箱内也无法执行脚本。
pub fn sanitize_html(html: &str) -> String {
    if html.trim().is_empty() {
        return String::new();
    }
    let out = DROP_BLOCK.replace_all(html, "");
    let out = DROP_ORPHAN.replace_all(&out, "");
    let out = DROP_EVENT_ATTR.replace_all(&out, "");
    let out = DROP_JS_URL.replace_all(&out, "");
    out.into_owned()
}

/// 判断清洗后的 HTML 是否还有可展示内容（避免只渲染出一堆空白标签）。
pub fn has_visible_content(html: &str) -> bool {
    let mut text = String::new();
    let mut in_tag = false;
    for ch in html.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => text.push(ch),
            _ => {}
        }
    }
    let cleaned = text
        .replace("&nbsp;", "")
        .replace('\u{200b}', "")
        .trim()
        .to_string();
    !cleaned.is_empty() || html.contains("<img")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 回归测试：所有 `LazyLock<Regex>` 都必须能成功编译。
    ///
    /// 这些正则在首次访问时初始化，一旦语法非法就会 panic 并**毒化** `LazyLock`，
    /// 之后每次访问都再 panic（表现为 "LazyLock instance has previously been poisoned"）。
    /// Rust 的 `regex` crate 不支持反向引用 `\1`，历史上这里就用过一次导致线上崩溃。
    #[test]
    fn all_regexes_compile() {
        // 依次强制初始化，任何一条非法都会在此测试中暴露。
        assert!(DROP_BLOCK.is_match("<script>x</script>"));
        assert!(DROP_ORPHAN.is_match("<iframe>"));
        assert!(DROP_EVENT_ATTR.is_match(" onclick=\"x\""));
        assert!(DROP_JS_URL.is_match("href=\"javascript:x\""));
    }

    #[test]
    fn drops_script_block_and_its_content() {
        let out = sanitize_html(r#"<p>a</p><script>alert(1)</script><p>b</p>"#);
        assert!(!out.contains("script"), "script 标签应被移除：{out}");
        assert!(!out.contains("alert"), "script 内容应被移除：{out}");
        assert!(out.contains("<p>a</p>") && out.contains("<p>b</p>"));
    }

    #[test]
    fn drops_nested_and_mismatched_containers() {
        // 无反向引用时无法精确配对，但结果是「所有危险容器及其内容都不残留」。
        let out = sanitize_html("<iframe><object>bad</object></iframe>ok");
        assert!(!out.contains("iframe"), "iframe 应被移除：{out}");
        assert!(!out.contains("object"), "object 应被移除：{out}");
        assert!(out.contains("ok"));
    }

    #[test]
    fn drops_unclosed_container() {
        let out = sanitize_html(r#"<div>hi<iframe src="http://x"></div>"#);
        assert!(!out.contains("iframe"), "未闭合的 iframe 应被移除：{out}");
        assert!(out.contains("hi"));
    }

    #[test]
    fn drops_event_handlers() {
        let out = sanitize_html(r#"<img src="a.png" onerror="alert(1)">"#);
        assert!(!out.contains("onerror"), "事件属性应被移除：{out}");
        assert!(out.contains("a.png"), "正常属性应保留：{out}");
    }

    #[test]
    fn drops_javascript_urls_quoted_and_unquoted() {
        let quoted = sanitize_html(r#"<a href="javascript:alert(1)">x</a>"#);
        assert!(!quoted.contains("javascript"), "带引号的 js URL 应被移除：{quoted}");

        let unquoted = sanitize_html("<a href=javascript:alert(1)>x</a>");
        assert!(
            !unquoted.contains("javascript"),
            "不带引号的 js URL 应被移除：{unquoted}"
        );
    }

    #[test]
    fn keeps_normal_html_intact() {
        let html = r#"<p>你好 <b>世界</b></p><img src="https://a.com/b.png">"#;
        let out = sanitize_html(html);
        assert!(out.contains("你好"));
        assert!(out.contains("<b>世界</b>"));
        assert!(out.contains("https://a.com/b.png"));
    }

    #[test]
    fn empty_input_is_empty() {
        assert_eq!(sanitize_html("   "), "");
    }

    #[test]
    fn has_visible_content_works() {
        assert!(!has_visible_content("<div>   </div>"));
        assert!(has_visible_content("<div>hi</div>"));
        assert!(has_visible_content("<img src=\"x.png\">"));
    }

    /// 真实邮件形态（Steam/Outlook 类营销邮件）不能被误伤：
    /// 含 `<!DOCTYPE>`、条件注释、大量 `<table>`/`<style>`、追踪像素等。
    #[test]
    fn real_world_newsletter_survives() {
        let html = r#"<!DOCTYPE html PUBLIC "-//W3C//DTD XHTML 1.0 Transitional//EN">
<html><head>
<meta http-equiv="Content-Type" content="text/html; charset=UTF-8">
<style type="text/css">.link{color:#66c0f4}</style>
<!--[if mso]><style>table{border:0}</style><![endif]-->
</head>
<body>
<table width="100%" cellpadding="0"><tr><td>
  <a href="https://store.steampowered.com/sale/x" class="link">秋季特卖现已盛大开幕</a>
</td></tr></table>
<img src="https://track.example.com/p.gif?u=1" width="1" height="1">
<p>还有专门为您准备的精选内容。</p>
</body></html>"#;

        let out = sanitize_html(html);

        // 正文与链接必须保留
        assert!(out.contains("秋季特卖现已盛大开幕"), "正文被误删：{out}");
        assert!(
            out.contains("https://store.steampowered.com/sale/x"),
            "正常链接被误删：{out}"
        );
        assert!(out.contains("还有专门为您准备的精选内容"), "尾部正文被误删");
        // <img> 追踪像素不是危险容器，不应被删（也不会执行脚本）
        assert!(out.contains("track.example.com"), "img 不应被删除：{out}");
        // 危险容器即使不存在也不应报错
        assert!(!out.contains("<script"));
    }

    /// 混合形态：正常容器中夹带危险容器，清洗后正常部分完整保留。
    #[test]
    fn mixed_content_keeps_safe_parts() {
        let html = r#"<div class="wrap">
  <p>第一段</p>
  <iframe src="//evil.com/x"></iframe>
  <p onclick="steal()">第二段</p>
  <a href="javascript:void(0)">恶意链接</a>
  <p>第三段</p>
</div>"#;

        let out = sanitize_html(html);

        assert!(out.contains("第一段"));
        assert!(out.contains("第二段"), "仅应去掉 onclick，文本要保留：{out}");
        assert!(out.contains("第三段"));
        assert!(!out.contains("evil.com"), "iframe 应被移除：{out}");
        assert!(!out.contains("onclick"), "事件属性应被移除：{out}");
        assert!(!out.contains("javascript:"), "js URL 应被移除：{out}");
    }
}
