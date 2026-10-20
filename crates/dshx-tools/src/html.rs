//! HTML → Markdown for `web_fetch`, following dsh's turndown setup: ATX
//! headings, fenced code, `-` bullets, GFM tables without span expansion, and
//! non-visible content (scripts, styles, hidden elements) removed first.
use std::rc::Rc;

use htmd::element_handler::{HandlerResult, Handlers};
use htmd::options::{BulletListMarker, CodeBlockStyle, HeadingStyle, Options};
use htmd::{Element, HtmlToMarkdown};
use markup5ever_rcdom::{Handle, NodeData};

const REMOVED_TAGS: [&str; 7] = [
    "script", "style", "noscript", "template", "iframe", "object", "embed",
];

fn attr<'a>(attrs: &'a [html5ever::Attribute], name: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|a| a.name.local.as_ref() == name)
        .map(|a| a.value.as_ref())
}

/// dsh's `removeNonVisibleContent` rule.
fn is_hidden(tag: &str, attrs: &[html5ever::Attribute]) -> bool {
    if REMOVED_TAGS.contains(&tag) {
        return true;
    }
    if attr(attrs, "hidden").is_some()
        || attr(attrs, "aria-hidden").is_some_and(|v| v.eq_ignore_ascii_case("true"))
    {
        return true;
    }
    if tag == "input" && attr(attrs, "type").is_some_and(|v| v.eq_ignore_ascii_case("hidden")) {
        return true;
    }
    attr(attrs, "style").is_some_and(|style| {
        style.split(';').any(|declaration| {
            let Some((property, value)) = declaration.split_once(':') else {
                return false;
            };
            let property = property.trim().to_ascii_lowercase();
            let value = value.trim().to_ascii_lowercase();
            let value = value
                .strip_suffix("!important")
                .map_or(value.as_str(), str::trim_end);
            (property == "display" && value == "none")
                || (property == "visibility" && (value == "hidden" || value == "collapse"))
        })
    })
}

fn prune(node: &Handle) {
    let mut children = node.children.borrow_mut();
    children.retain(|child| match &child.data {
        NodeData::Element { name, attrs, .. } => !is_hidden(name.local.as_ref(), &attrs.borrow()),
        NodeData::Comment { .. } => false,
        _ => true,
    });
    for child in children.iter() {
        prune(child);
    }
    // <template> contents live outside the child list; nothing to walk there.
}

fn element_children(node: &Rc<htmd::Node>) -> Vec<Rc<htmd::Node>> {
    node.children
        .borrow()
        .iter()
        .filter(|c| matches!(c.data, NodeData::Element { .. }))
        .cloned()
        .collect()
}

fn tag_of(node: &Rc<htmd::Node>) -> String {
    match &node.data {
        NodeData::Element { name, .. } => name.local.as_ref().to_owned(),
        _ => String::new(),
    }
}

/// dsh `renderTableCell`.
fn table_cell(content: &str, index: usize) -> String {
    let prefix = if index == 0 { "| " } else { " " };
    let mut escaped = content.trim().replace("\n\r", "<br>").replace('\n', "<br>");
    // Runs of `|` collapse into one escaped bar.
    let mut out = String::new();
    let mut in_bars = false;
    for ch in escaped.drain(..) {
        if ch == '|' {
            if !in_bars {
                out.push_str("\\|");
            }
            in_bars = true;
        } else {
            in_bars = false;
            out.push(ch);
        }
    }
    while out.chars().count() < 3 {
        out.push(' ');
    }
    format!("{prefix}{out} |")
}

fn cell_index(cell: &Rc<htmd::Node>) -> usize {
    let Some(parent) = cell.parent.take() else {
        return 0;
    };
    cell.parent.set(Some(parent.clone()));
    let Some(row) = parent.upgrade() else {
        return 0;
    };
    let children = row.children.borrow();
    children
        .iter()
        .position(|c| Rc::ptr_eq(c, cell))
        .unwrap_or(0)
}

fn parent_of(node: &Rc<htmd::Node>) -> Option<Rc<htmd::Node>> {
    let weak = node.parent.take()?;
    node.parent.set(Some(weak.clone()));
    weak.upgrade()
}

fn is_heading_row(row: &Rc<htmd::Node>) -> bool {
    let cells = element_children(row);
    let Some(section) = parent_of(row) else {
        return false;
    };
    let first_row = parent_of(&section).and_then(|table| {
        let mut rows = Vec::new();
        for child in element_children(&table) {
            match tag_of(&child).as_str() {
                "tr" => rows.push(child),
                "thead" | "tbody" | "tfoot" => rows.extend(
                    element_children(&child)
                        .into_iter()
                        .filter(|r| tag_of(r) == "tr"),
                ),
                _ => {}
            }
        }
        rows.into_iter().next()
    });
    (tag_of(&section) == "thead" || first_row.is_some_and(|r| Rc::ptr_eq(&r, row)))
        && !cells.is_empty()
        && cells.iter().all(|c| tag_of(c) == "th")
}

fn border(cell: &Rc<htmd::Node>) -> &'static str {
    let NodeData::Element { attrs, .. } = &cell.data else {
        return "---";
    };
    let attrs = attrs.borrow();
    let mut align = attr(&attrs, "align").unwrap_or("").to_ascii_lowercase();
    if align.is_empty()
        && let Some(style) = attr(&attrs, "style")
    {
        for declaration in style.split(';') {
            if let Some((p, v)) = declaration.split_once(':')
                && p.trim().eq_ignore_ascii_case("text-align")
            {
                align = v.trim().to_ascii_lowercase();
            }
        }
    }
    match align.as_str() {
        "left" => ":---",
        "right" => "---:",
        "center" => ":---:",
        _ => "---",
    }
}

fn converter() -> HtmlToMarkdown {
    let options = Options {
        heading_style: HeadingStyle::Atx,
        code_block_style: CodeBlockStyle::Fenced,
        bullet_list_marker: BulletListMarker::Dash,
        ..Options::default()
    };
    HtmlToMarkdown::builder()
        .options(options)
        // turndown's default emphasis delimiter is `_`.
        .add_handler(
            vec!["em", "i"],
            |handlers: &dyn Handlers, element: Element| -> Option<HandlerResult> {
                let content = handlers.walk_children(element.node).content;
                let trimmed = content.trim_matches(|c: char| c.is_ascii_whitespace());
                if trimmed.is_empty() {
                    return None;
                }
                let start = content.len()
                    - content
                        .trim_start_matches(|c: char| c.is_ascii_whitespace())
                        .len();
                let end = content
                    .trim_end_matches(|c: char| c.is_ascii_whitespace())
                    .len();
                Some(format!("{}_{trimmed}_{}", &content[..start], &content[end..]).into())
            },
        )
        .add_handler(
            vec!["th", "td"],
            |handlers: &dyn Handlers, element: Element| -> Option<HandlerResult> {
                let content = handlers.walk_children(element.node).content;
                Some(table_cell(&content, cell_index(element.node)).into())
            },
        )
        .add_handler(
            vec!["tr"],
            |handlers: &dyn Handlers, element: Element| -> Option<HandlerResult> {
                let content = handlers.walk_children(element.node).content;
                let border = if is_heading_row(element.node) {
                    element_children(element.node)
                        .iter()
                        .enumerate()
                        .map(|(i, cell)| table_cell(border(cell), i))
                        .collect::<String>()
                } else {
                    String::new()
                };
                let text = if border.is_empty() {
                    format!("\n{content}")
                } else {
                    format!("\n{content}\n{border}")
                };
                Some(text.into())
            },
        )
        .add_handler(
            vec!["thead", "tbody", "tfoot"],
            |handlers: &dyn Handlers, element: Element| -> Option<HandlerResult> {
                Some(handlers.walk_children(element.node).content.into())
            },
        )
        .add_handler(
            vec!["table"],
            |handlers: &dyn Handlers, element: Element| -> Option<HandlerResult> {
                let content = handlers.walk_children(element.node).content;
                let content = content.trim_matches('\n');
                Some(format!("\n\n{content}\n\n").into())
            },
        )
        .build()
}

/// Converts an HTML document; `None` when conversion fails.
pub fn html_to_markdown(html: &str) -> Option<String> {
    let converter = converter();
    let tree = converter.html_to_tree(html).ok()?;
    prune(&tree);
    Some(converter.tree_to_markdown(&tree))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dsh_web_fetch_fixture() {
        // snapshots/session/web-fetch in the dsh repo.
        let page = r#"<!doctype html>
<html><head><title>Menu</title><style>.x{color:red}</style><script>ignored()</script></head>
<body>
<h1>Caf&eacute; menu</h1>
<p>Prices include <strong>service &amp; <em>tax</em></strong> &mdash; updated daily.</p>
<ul><li>Espresso</li><li>Flat white</li></ul>
<table><thead><tr><th>Drink</th><th>Price</th></tr></thead><tbody><tr><td>Espresso</td><td>&euro;2</td></tr><tr><td>Flat white</td><td>&euro;3</td></tr></tbody></table>
<p>See <a href="https://fixture.invalid/specials">today&rsquo;s specials</a>.</p>
</body></html>
"#;
        let expected = "Menu\n\n# Café menu\n\nPrices include **service & _tax_** — updated daily.\n\n-   Espresso\n-   Flat white\n\n| Drink | Price |\n| --- | --- |\n| Espresso | €2  |\n| Flat white | €3  |\n\nSee [today’s specials](https://fixture.invalid/specials).";
        assert_eq!(html_to_markdown(page).unwrap(), expected);
    }

    #[test]
    fn hidden_content_is_removed() {
        let page = r#"<p>a</p><div hidden>b</div><p style="display: none !important">c</p><span aria-hidden="true">d</span><input type="hidden" value="e"><p>f</p>"#;
        assert_eq!(html_to_markdown(page).unwrap(), "a\n\nf");
    }
}
