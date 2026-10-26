//! Generated content: what `::before` / `::after` put in the page, and how `content` computes
//! (CSS Generated Content 3).
//!
//! Measured on `/usr/bin/google-chrome` 150.0.7871.46 over loopback (the unit's `pseudo.html`):
//!
//! ```text
//! #a::before { content: "PRE"; display: inline-block; width: 40px; height: 24px }
//!   getComputedStyle(a, "::before").content     "\"PRE\""       .width "40px"   a.offsetHeight 24
//!   getComputedStyle(a, "::after").content      "none"   (no rule: `normal` computes to `none`)
//!   a.innerText / a.textContent / childNodes    "text" / "text" / 1  -- generated content is in neither
//! #c::after { content: attr(data-x) " " counter(n) " " open-quote "q" close-quote }   data-x="ATTR"
//!   content  "\"ATTR \" counter(n) \" \" open-quote \"q\" close-quote"
//! li::marker content                            "normal"
//! #b::after { content: url(pseudo-content.png) }  "url(\"http://127.0.0.1:…/pseudo-content.png\")"
//! ```
//!
//! So `attr()` is substituted at computed-value time and the string it yields joins the string next
//! to it; `counter()`, the quotes and `url()` stay symbolic in the computed value and are resolved
//! only when the box is built.

use cssparser::{Parser, ParserInput, Token};
use korvus_dom::{Dom, NodeId};

/// The pseudo-elements that generate a box from `content`.
pub fn generates_content(pseudo: &str) -> bool {
    matches!(pseudo, "before" | "after")
}

/// One item of a computed `content` list.
#[derive(Debug, Clone, PartialEq)]
pub enum ContentItem {
    Text(String),
    Url(String),
    Counter { name: String, style: Option<String> },
    Counters { name: String, separator: String, style: Option<String> },
    OpenQuote,
    CloseQuote,
    NoOpenQuote,
    NoCloseQuote,
    /// A function this engine keeps as written (`image-set()`, a gradient).
    Other(String),
}

/// The computed `content` of `pseudo` on `node`, as `getComputedStyle` serializes it.
///
/// `specified` is the cascaded value. `normal` computes to `none` on `::before`/`::after` and stays
/// `normal` elsewhere. `url()`s have already been made absolute by the cascade when their sheet has a
/// base; `base` is the document's, for the rest.
pub fn computed_content(pseudo: &str, specified: &str, dom: &Dom, node: NodeId, base: Option<&str>) -> String {
    let t = specified.trim();
    if t.eq_ignore_ascii_case("normal") {
        return if generates_content(pseudo) { "none".to_string() } else { "normal".to_string() };
    }
    if t.eq_ignore_ascii_case("none") {
        return "none".to_string();
    }
    match parse_content(t, dom, node, base) {
        Some((items, alt)) => {
            let mut out = serialize_items(&items);
            if let Some(alt) = alt {
                out.push_str(" / ");
                out.push_str(&serialize_items(&alt));
            }
            out
        }
        None => t.to_string(),
    }
}

/// The items of a COMPUTED `content` value (what [`computed_content`] produced), for building the
/// box. `None` for `none` / `normal`, which generate nothing.
pub fn content_items(computed: &str) -> Option<Vec<ContentItem>> {
    let t = computed.trim();
    if t.eq_ignore_ascii_case("none") || t.eq_ignore_ascii_case("normal") || t.is_empty() {
        return None;
    }
    // A computed value has no `attr()` left, so no element is needed to read it back.
    let dom = Dom::empty();
    parse_content(t, &dom, dom.root(), None).map(|(items, _)| items)
}

fn serialize_string(s: &str) -> String {
    let mut out = String::new();
    let _ = cssparser::serialize_string(s, &mut out);
    out
}

fn serialize_items(items: &[ContentItem]) -> String {
    items
        .iter()
        .map(|i| match i {
            ContentItem::Text(s) => serialize_string(s),
            ContentItem::Url(u) => format!("url({})", serialize_string(u)),
            ContentItem::Counter { name, style: None } => format!("counter({name})"),
            ContentItem::Counter { name, style: Some(s) } => format!("counter({name}, {s})"),
            ContentItem::Counters { name, separator, style: None } => format!("counters({name}, {})", serialize_string(separator)),
            ContentItem::Counters { name, separator, style: Some(s) } => {
                format!("counters({name}, {}, {s})", serialize_string(separator))
            }
            ContentItem::OpenQuote => "open-quote".to_string(),
            ContentItem::CloseQuote => "close-quote".to_string(),
            ContentItem::NoOpenQuote => "no-open-quote".to_string(),
            ContentItem::NoCloseQuote => "no-close-quote".to_string(),
            ContentItem::Other(s) => s.clone(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

type Parsed = (Vec<ContentItem>, Option<Vec<ContentItem>>);

fn parse_content(text: &str, dom: &Dom, node: NodeId, base: Option<&str>) -> Option<Parsed> {
    let mut pi = ParserInput::new(text);
    let mut p = Parser::new(&mut pi);
    let mut items: Vec<ContentItem> = Vec::new();
    let mut alt: Option<Vec<ContentItem>> = None;
    let push = |list: &mut Vec<ContentItem>, item: ContentItem| {
        // Adjacent strings are one string: `attr(x) " "` is `"ATTR "` in Chrome.
        if let (ContentItem::Text(add), Some(ContentItem::Text(prev))) = (&item, list.last_mut()) {
            prev.push_str(add);
            return;
        }
        list.push(item);
    };
    loop {
        let token = match p.next() {
            Ok(t) => t.clone(),
            Err(_) => break,
        };
        if matches!(token, Token::Delim('/')) && alt.is_none() {
            alt = Some(Vec::new());
            continue;
        }
        let target: &mut Vec<ContentItem> = match alt.as_mut() {
            Some(a) => a,
            None => &mut items,
        };
        match token {
            Token::QuotedString(s) => push(target, ContentItem::Text(s.to_string())),
            Token::UnquotedUrl(u) => {
                let url = absolutize(&u, base);
                push(target, ContentItem::Url(url))
            }
            Token::Ident(i) => {
                let item = match i.to_ascii_lowercase().as_str() {
                    "open-quote" => ContentItem::OpenQuote,
                    "close-quote" => ContentItem::CloseQuote,
                    "no-open-quote" => ContentItem::NoOpenQuote,
                    "no-close-quote" => ContentItem::NoCloseQuote,
                    _ => return None,
                };
                push(target, item)
            }
            Token::Function(name) => {
                let lower = name.to_ascii_lowercase();
                let start = p.position();
                let args: Vec<String> = p
                    .parse_nested_block(|p| -> Result<Vec<String>, cssparser::ParseError<'_, ()>> {
                        let mut out = Vec::new();
                        let mut cur = String::new();
                        while let Ok(t) = p.next_including_whitespace() {
                            match t {
                                Token::Comma => out.push(std::mem::take(&mut cur)),
                                Token::QuotedString(s) => cur.push_str(&format!("\u{0}{s}")),
                                Token::WhiteSpace(_) => cur.push(' '),
                                t => {
                                    let mut tmp = String::new();
                                    let _ = cssparser::ToCss::to_css(t, &mut tmp);
                                    cur.push_str(&tmp);
                                }
                            }
                        }
                        out.push(cur);
                        Ok(out)
                    })
                    .ok()?
                    .into_iter()
                    .map(|a| a.trim().to_string())
                    .collect();
                let raw = |a: &str| a.strip_prefix('\u{0}').map(str::to_string);
                let item = match lower.as_str() {
                    "attr" => {
                        let attr = args.first()?.split_whitespace().next()?.to_string();
                        let value = dom.get_attribute(node, &attr).unwrap_or("").to_string();
                        ContentItem::Text(value)
                    }
                    "url" => ContentItem::Url(absolutize(&raw(args.first()?)?, base)),
                    "counter" => ContentItem::Counter {
                        name: args.first()?.clone(),
                        style: args.get(1).filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("decimal")).cloned(),
                    },
                    "counters" => ContentItem::Counters {
                        name: args.first()?.clone(),
                        separator: raw(args.get(1)?)?,
                        style: args.get(2).filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("decimal")).cloned(),
                    },
                    _ => {
                        let _ = start;
                        ContentItem::Other(format!("{name}({})", args.join(", ").replace('\u{0}', "")))
                    }
                };
                push(target, item)
            }
            Token::WhiteSpace(_) => {}
            _ => return None,
        }
    }
    Some((items, alt))
}

fn absolutize(url: &str, base: Option<&str>) -> String {
    match base {
        Some(b) if !url.contains("://") && !url.starts_with("data:") => crate::style::urls::resolve(b, url),
        _ => url.to_string(),
    }
}

/// Counter values in scope at one element, and the quote nesting depth, for turning the symbolic
/// items into the text a box lays out.
pub struct ContentScope<'a> {
    pub counter: &'a dyn Fn(&str) -> Vec<i32>,
    pub quote_depth: &'a mut i32,
}

/// The text `items` renders as. `url()` images contribute no text (and no size: this engine decodes
/// no image into a pseudo-element), which is Chrome's answer for an image that fails to decode too.
pub fn render_text(items: &[ContentItem], scope: &mut ContentScope) -> String {
    let mut out = String::new();
    for item in items {
        match item {
            ContentItem::Text(s) => out.push_str(s),
            ContentItem::Counter { name, style } => {
                let v = (scope.counter)(name).last().copied().unwrap_or(0);
                out.push_str(&format_counter(v, style.as_deref()));
            }
            ContentItem::Counters { name, separator, style } => {
                let vs = (scope.counter)(name);
                let vs = if vs.is_empty() { vec![0] } else { vs };
                out.push_str(&vs.iter().map(|v| format_counter(*v, style.as_deref())).collect::<Vec<_>>().join(separator));
            }
            ContentItem::OpenQuote => {
                out.push_str(if *scope.quote_depth % 2 == 0 { "\u{201C}" } else { "\u{2018}" });
                *scope.quote_depth += 1;
            }
            ContentItem::CloseQuote => {
                if *scope.quote_depth > 0 {
                    *scope.quote_depth -= 1;
                    out.push_str(if *scope.quote_depth % 2 == 0 { "\u{201D}" } else { "\u{2019}" });
                }
            }
            ContentItem::NoOpenQuote => *scope.quote_depth += 1,
            ContentItem::NoCloseQuote => *scope.quote_depth = (*scope.quote_depth - 1).max(0),
            ContentItem::Url(_) | ContentItem::Other(_) => {}
        }
    }
    out
}

/// A counter value in a list-style type. The common styles; any other is decimal.
pub fn format_counter(v: i32, style: Option<&str>) -> String {
    let alpha = |v: i32, base: u8| -> String {
        if v <= 0 {
            return v.to_string();
        }
        let mut n = v;
        let mut s = Vec::new();
        while n > 0 {
            n -= 1;
            s.push(base + (n % 26) as u8);
            n /= 26;
        }
        s.reverse();
        String::from_utf8(s).unwrap_or_default()
    };
    match style.map(|s| s.to_ascii_lowercase()) {
        Some(s) if s == "lower-alpha" || s == "lower-latin" => alpha(v, b'a'),
        Some(s) if s == "upper-alpha" || s == "upper-latin" => alpha(v, b'A'),
        Some(s) if s == "lower-roman" => roman(v).to_ascii_lowercase(),
        Some(s) if s == "upper-roman" => roman(v),
        Some(s) if s == "decimal-leading-zero" => format!("{v:02}"),
        Some(s) if s == "none" => String::new(),
        Some(s) if s == "disc" => "\u{2022}".to_string(),
        Some(s) if s == "circle" => "\u{25E6}".to_string(),
        Some(s) if s == "square" => "\u{25AA}".to_string(),
        _ => v.to_string(),
    }
}

fn roman(v: i32) -> String {
    if !(1..4000).contains(&v) {
        return v.to_string();
    }
    let table = [(1000, "M"), (900, "CM"), (500, "D"), (400, "CD"), (100, "C"), (90, "XC"), (50, "L"), (40, "XL"), (10, "X"), (9, "IX"), (5, "V"), (4, "IV"), (1, "I")];
    let mut n = v;
    let mut out = String::new();
    for (value, s) in table {
        while n >= value {
            out.push_str(s);
            n -= value;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attr_is_substituted_and_joins_the_string_after_it() {
        let dom = Dom::parse("<div id=c data-x=ATTR></div>");
        let node = (0..dom.len() as u32).find(|&n| dom.get_attribute(n, "id") == Some("c")).unwrap();
        assert_eq!(
            computed_content("after", "attr(data-x) \" \" counter(n) \" \" open-quote \"q\" close-quote", &dom, node, None),
            "\"ATTR \" counter(n) \" \" open-quote \"q\" close-quote"
        );
        assert_eq!(computed_content("before", "normal", &dom, node, None), "none");
        assert_eq!(computed_content("marker", "normal", &dom, node, None), "normal");
        assert_eq!(computed_content("before", "'PRE'", &dom, node, None), "\"PRE\"");
    }

    #[test]
    fn counters_and_quotes_render() {
        let items = content_items("counter(n) \". \" open-quote \"x\" close-quote").unwrap();
        let mut depth = 0;
        let counter = |_: &str| vec![5];
        let mut scope = ContentScope { counter: &counter, quote_depth: &mut depth };
        assert_eq!(render_text(&items, &mut scope), "5. \u{201C}x\u{201D}");
    }
}
