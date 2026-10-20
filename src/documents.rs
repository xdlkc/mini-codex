//! Structured local documents. Desktop owns the single locked disk transaction.
use serde_json::{json, Value};
use std::collections::HashSet;
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) type Result<T> = std::result::Result<T, String>;
const MAX_CURRENT: usize = 8 * 1024 * 1024;
const MAX_HISTORY: usize = 32 * 1024 * 1024;
const MAX_VERSIONS: usize = 100;
const MAX_NODES: usize = 100_000;
const MAX_DEPTH: usize = 64;
const MAX_COMMENTS: usize = 1000;
const MAX_COMMENT: usize = 16 * 1024;
pub(crate) const METHODS: &[&str] = &[
    "desktop/document/create",
    "desktop/document/read",
    "desktop/document/update",
    "desktop/document/delete",
    "desktop/document/list",
    "desktop/document/history/list",
    "desktop/document/history/read",
    "desktop/document/restore",
    "desktop/document/export",
    "desktop/document/comments/list",
    "desktop/document/comments/create",
    "desktop/document/comments/update",
    "desktop/document/comments/delete",
];
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
fn new_id(prefix: &str) -> String {
    format!("{prefix}_{:032x}", rand::random::<u128>())
}
fn string<'a>(p: &'a Value, key: &str) -> Result<&'a str> {
    p[key]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("{key} must be a non-empty string"))
}
fn integer(p: &Value, key: &str) -> Result<u64> {
    p[key]
        .as_u64()
        .ok_or_else(|| format!("{key} must be a nonnegative integer"))
}
fn fields(p: &Value, allowed: &[&str]) -> Result<()> {
    let object = p
        .as_object()
        .ok_or("Document parameters must be an object")?;
    for key in object.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(format!("Unknown document field: {key}"));
        }
    }
    Ok(())
}
fn safe_string(s: &str, max: usize) -> bool {
    s.len() <= max
        && !s
            .chars()
            .any(|c| c == '\0' || (c.is_control() && !matches!(c, '\n' | '\r' | '\t')))
}
fn attachment_id(s: &str) -> bool {
    s.len() == 36 && s.starts_with("att_") && s[4..].bytes().all(|b| b.is_ascii_hexdigit())
}
fn attrs<'a>(
    node: &'a Value,
    allowed: &[&str],
) -> Result<Option<&'a serde_json::Map<String, Value>>> {
    match node.get("attrs") {
        None => Ok(None),
        Some(Value::Object(object)) => {
            for key in object.keys() {
                if !allowed.contains(&key.as_str()) {
                    return Err(format!(
                        "Unsupported {} attribute: {key}",
                        node["type"].as_str().unwrap_or("node")
                    ));
                }
            }
            Ok(Some(object))
        }
        _ => Err("Node attrs must be an object".into()),
    }
}
fn positive(node: &Value, key: &str, default: u64, max: u64) -> Result<u64> {
    match node["attrs"].get(key) {
        None => Ok(default),
        Some(value) => value
            .as_u64()
            .filter(|n| *n > 0 && *n <= max)
            .ok_or_else(|| format!("{key} must be a positive integer up to {max}")),
    }
}
fn attr_string<'a>(
    node: &'a Value,
    key: &str,
    default: &'a str,
    max: usize,
    nullable: bool,
) -> Result<&'a str> {
    match node["attrs"].get(key) {
        None => Ok(default),
        Some(Value::Null) if nullable => Ok(default),
        Some(Value::String(s)) if safe_string(s, max) => Ok(s),
        _ => Err(format!("Invalid {key} attribute")),
    }
}
fn children(node: &Value) -> Result<&[Value]> {
    match node.get("content") {
        None => Ok(&[]),
        Some(Value::Array(items)) => Ok(items),
        _ => Err("Node content must be an array".into()),
    }
}
fn kind(node: &Value) -> Result<&str> {
    string(node, "type")
}
fn block(name: &str) -> bool {
    matches!(
        name,
        "paragraph"
            | "heading"
            | "blockquote"
            | "bulletList"
            | "orderedList"
            | "taskList"
            | "codeBlock"
            | "horizontalRule"
            | "table"
            | "image"
            | "fileAttachment"
            | "blockMath"
            | "workBlock"
    )
}
fn inline(name: &str) -> bool {
    matches!(name, "text" | "hardBreak" | "inlineMath")
}
fn leaf(name: &str) -> bool {
    matches!(
        name,
        "hardBreak"
            | "horizontalRule"
            | "image"
            | "fileAttachment"
            | "inlineMath"
            | "blockMath"
            | "workBlock"
    )
}
fn valid_link(href: &str) -> bool {
    if !safe_string(href, 4096) || href.chars().any(char::is_whitespace) {
        return false;
    }
    reqwest::Url::parse(href).is_ok_and(|url| match url.scheme() {
        "http" | "https" => url.has_host() && url.username().is_empty() && url.password().is_none(),
        "mailto" => !url.path().is_empty(),
        _ => false,
    })
}
fn validate_marks(node: &Value, name: &str) -> Result<()> {
    let Some(marks) = node.get("marks") else {
        return Ok(());
    };
    let marks = marks.as_array().ok_or("marks must be an array")?;
    if !marks.is_empty() && !matches!(name, "text" | "inlineMath" | "hardBreak") {
        return Err("Marks can only be applied to inline nodes".into());
    }
    let mut seen = HashSet::new();
    for mark in marks {
        fields(mark, &["type", "attrs"])?;
        let ty = kind(mark)?;
        if !seen.insert(ty) {
            return Err("Duplicate text mark".into());
        }
        match ty {
            "bold" | "italic" | "strike" | "underline" | "code" => {
                attrs(mark, &[])?;
            }
            "link" => {
                attrs(mark, &["href"])?;
                if !valid_link(attr_string(mark, "href", "", 4096, false)?) {
                    return Err(
                        "Links must use http, https or mailto without embedded credentials".into(),
                    );
                }
            }
            _ => return Err(format!("Unsupported text mark: {ty}")),
        }
    }
    if seen.contains("code") && seen.len() != 1 {
        return Err("Code marks cannot be combined with other marks".into());
    }
    Ok(())
}
fn validate_node(node: &Value, depth: usize, count: &mut usize) -> Result<()> {
    if depth > MAX_DEPTH {
        return Err("Document nesting exceeds 64 levels".into());
    }
    *count += 1;
    if *count > MAX_NODES {
        return Err("Document exceeds 100000 nodes".into());
    }
    fields(node, &["type", "attrs", "content", "text", "marks"])?;
    let name = kind(node)?;
    let items = children(node)?;
    if name != "text" && node.get("text").is_some() {
        return Err("Only text nodes may contain text".into());
    }
    match name {
        "doc" | "paragraph" | "text" | "blockquote" | "bulletList" | "listItem" | "taskList"
        | "hardBreak" | "horizontalRule" | "tableRow" => {
            attrs(node, &[])?;
        }
        "heading" => {
            attrs(node, &["level"])?;
            positive(node, "level", 1, 6)?;
        }
        "orderedList" => {
            attrs(node, &["start", "type"])?;
            positive(node, "start", 1, 1_000_000_000)?;
            if let Some(value) = node["attrs"].get("type") {
                if !value.is_null() && !matches!(value.as_str(), Some("1" | "a" | "A" | "i" | "I"))
                {
                    return Err("Invalid ordered-list type".into());
                }
            }
        }
        "codeBlock" => {
            attrs(node, &["language"])?;
            let language = attr_string(node, "language", "", 128, true)?;
            if language.chars().any(char::is_control) {
                return Err("Code language cannot contain control characters".into());
            }
        }
        "taskItem" => {
            attrs(node, &["checked"])?;
            if node["attrs"]
                .get("checked")
                .is_some_and(|v| !v.is_boolean())
            {
                return Err("Task checked must be boolean".into());
            }
        }
        "table" => {
            attrs(node, &["layout"])?;
            if !matches!(
                node["attrs"]
                    .get("layout")
                    .and_then(Value::as_str)
                    .unwrap_or("normal"),
                "normal" | "flexible" | "full"
            ) {
                return Err("Invalid table layout".into());
            }
        }
        "tableCell" | "tableHeader" => {
            attrs(node, &["colspan", "rowspan", "colwidth", "align"])?;
            let colspan = positive(node, "colspan", 1, 1000)?;
            positive(node, "rowspan", 1, 1000)?;
            if let Some(width) = node["attrs"].get("colwidth").filter(|v| !v.is_null()) {
                let widths = width
                    .as_array()
                    .ok_or("colwidth must be an array or null")?;
                if widths.len() != colspan as usize
                    || widths
                        .iter()
                        .any(|w| w.as_u64().is_none_or(|n| n == 0 || n > 100_000))
                {
                    return Err("colwidth must have one positive width per spanned column".into());
                }
            }
            if let Some(align) = node["attrs"].get("align").filter(|v| !v.is_null()) {
                if !matches!(align.as_str(), Some("left" | "center" | "right")) {
                    return Err("Invalid table-cell alignment".into());
                }
            }
        }
        "image" | "fileAttachment" => {
            attrs(
                node,
                if name == "image" {
                    &["attachmentId", "name", "alt", "title", "width", "height"]
                } else {
                    &["attachmentId", "name", "mimeType", "size"]
                },
            )?;
            if !attachment_id(attr_string(node, "attachmentId", "", 100, false)?) {
                return Err("Image and file blocks require a stored attachmentId".into());
            }
            attr_string(node, "name", "", 1024, false)?;
            if name == "image" {
                attr_string(node, "alt", "", 4096, false)?;
                attr_string(node, "title", "", 4096, false)?;
                for key in ["width", "height"] {
                    if node["attrs"].get(key).is_some_and(|v| {
                        !v.is_null() && v.as_u64().is_none_or(|n| n == 0 || n > 100_000)
                    }) {
                        return Err(format!("Invalid image {key}"));
                    }
                }
            } else {
                attr_string(node, "mimeType", "application/octet-stream", 255, false)?;
                if node["attrs"]
                    .get("size")
                    .is_some_and(|v| !v.is_null() && !v.is_u64())
                {
                    return Err("File size must be a nonnegative integer or null".into());
                }
            }
        }
        "inlineMath" | "blockMath" => {
            attrs(node, &["latex"])?;
            attr_string(node, "latex", "", 65536, false)?;
        }
        "workBlock" => {
            attrs(node, &["kind", "title", "text", "targetId", "html"])?;
            let kind = attr_string(node, "kind", "prompt", 32, false)?;
            if !matches!(
                kind,
                "prompt" | "task" | "instructions" | "highlight" | "page" | "chat" | "visualize"
            ) {
                return Err("Unsupported local Work block".into());
            }
            attr_string(node, "title", "", 1024, false)?;
            attr_string(node, "text", "", 65536, false)?;
            attr_string(node, "html", "", 65536, false)?;
            let target = attr_string(node, "targetId", "", 256, false)?;
            if matches!(kind, "page" | "chat") && target.is_empty() {
                return Err("A local reference ID is required".into());
            }
        }
        _ => return Err(format!("Unsupported document node: {name}")),
    }
    if name == "text" {
        let value = node["text"]
            .as_str()
            .filter(|s| !s.is_empty() && safe_string(s, MAX_CURRENT))
            .ok_or("Text nodes require non-empty valid text")?;
        let _ = value;
        if !items.is_empty() {
            return Err("Text nodes cannot have child nodes".into());
        }
    }
    if leaf(name) && !items.is_empty() {
        return Err("Leaf nodes cannot have child nodes".into());
    }
    validate_marks(node, name)?;
    for (index, child) in items.iter().enumerate() {
        let child_name = kind(child)?;
        let allowed = match name {
            "doc" | "blockquote" => block(child_name),
            "paragraph" | "heading" => inline(child_name),
            "codeBlock" => {
                child_name == "text"
                    && child
                        .get("marks")
                        .is_none_or(|v| v.as_array().is_some_and(Vec::is_empty))
            }
            "bulletList" | "orderedList" => child_name == "listItem",
            "taskList" => child_name == "taskItem",
            "listItem" | "taskItem" => {
                if index == 0 {
                    child_name == "paragraph"
                } else {
                    block(child_name)
                }
            }
            "table" => child_name == "tableRow",
            "tableRow" => matches!(child_name, "tableCell" | "tableHeader"),
            "tableCell" | "tableHeader" => block(child_name),
            _ => false,
        };
        if !allowed {
            return Err(format!("Invalid {child_name} inside {name}"));
        }
        validate_node(child, depth + 1, count)?;
    }
    if matches!(
        name,
        "doc"
            | "blockquote"
            | "bulletList"
            | "orderedList"
            | "taskList"
            | "listItem"
            | "taskItem"
            | "table"
            | "tableCell"
            | "tableHeader"
    ) && items.is_empty()
    {
        return Err(format!("{name} cannot be empty"));
    }
    if name == "table" {
        validate_table(items)?;
    }
    Ok(())
}
// Validate the same rectangular grid that the editor's table map uses. Bounds also
// prevent tiny input with huge spans from allocating an unbounded grid.
fn validate_table(rows: &[Value]) -> Result<()> {
    if rows.len() > 1000 {
        return Err("Tables can contain up to 1000 rows".into());
    }
    let mut occupied = vec![Vec::<bool>::new(); rows.len()];
    let mut width = 0;
    for (row_index, row) in rows.iter().enumerate() {
        let mut column = 0;
        for cell in children(row)? {
            while occupied[row_index].get(column) == Some(&true) {
                column += 1;
            }
            let columns = positive(cell, "colspan", 1, 1000)? as usize;
            let height = positive(cell, "rowspan", 1, 1000)? as usize;
            let right = column + columns;
            if right > 1000 || row_index + height > rows.len() {
                return Err("Table span exceeds its row or 1000-column boundary".into());
            }
            for cells in occupied.iter_mut().skip(row_index).take(height) {
                cells.resize(cells.len().max(right), false);
                for slot in &mut cells[column..right] {
                    if *slot {
                        return Err("Overlapping table cells".into());
                    }
                    *slot = true;
                }
            }
            column = right;
        }
        width = width.max(occupied[row_index].len());
    }
    if width == 0
        || occupied
            .iter()
            .any(|row| row.len() != width || row.contains(&false))
    {
        return Err("Table rows must form a rectangular grid without missing cells".into());
    }
    Ok(())
}
pub(crate) fn validate_body(body: &Value) -> Result<()> {
    if body["type"] != "doc" {
        return Err("Rich body must be a ProseMirror doc".into());
    }
    if serde_json::to_vec(body).map_err(|e| e.to_string())?.len() > MAX_CURRENT {
        return Err("Document body exceeds 8 MiB".into());
    }
    validate_node(body, 0, &mut 0)
}
fn text_size(text: &str) -> u64 {
    text.encode_utf16().count() as u64
}
fn node_size(node: &Value) -> u64 {
    let name = node["type"].as_str().unwrap_or("");
    if name == "text" {
        return text_size(node["text"].as_str().unwrap_or(""));
    }
    if leaf(name) {
        return 1;
    }
    let size = node["content"]
        .as_array()
        .into_iter()
        .flatten()
        .map(node_size)
        .sum::<u64>();
    if name == "doc" {
        size
    } else {
        size + 2
    }
}
fn utf16_slice(text: &str, from: u64, to: u64) -> Result<String> {
    let mut units = 0;
    let mut start = None;
    for (byte, character) in text.char_indices() {
        if units == from {
            start = Some(byte);
        }
        if units == to {
            return Ok(text[start.ok_or("Selection splits a UTF-16 surrogate pair")?..byte].into());
        }
        let next = units + character.len_utf16() as u64;
        if units < from && from < next || units < to && to < next {
            return Err("Selection splits a UTF-16 surrogate pair".into());
        }
        units = next;
    }
    if units == from {
        start = Some(text.len());
    }
    if units != to {
        return Err("Selection is outside its text node".into());
    }
    Ok(text[start.ok_or("Selection is outside its text node")?..].into())
}
/// Same text as PM doc.textBetween(from,to,"\n","\uFFFC"). Never reads attachments.
pub(crate) fn selection_text(document: &Value, from: u64, to: u64) -> Result<String> {
    if document["format"] != "richtext" {
        return Err("Selection ranges require a richtext document".into());
    }
    let body = &document["body"];
    validate_body(body)?;
    selection_in_valid_body(body, from, to)
}
pub(crate) fn insertion_position(document: &Value, position: u64) -> Result<()> {
    if document["format"] != "richtext" {
        return Err("Insertion requires a richtext document".into());
    }
    let body = &document["body"];
    validate_body(body)?;
    if position > node_size(body) {
        return Err("Insertion is outside the document".into());
    }
    fn check(node: &Value, start: u64, position: u64) -> Result<()> {
        if node["type"] == "text" && position >= start && position <= start + node_size(node) {
            utf16_slice(
                node["text"].as_str().unwrap(),
                position - start,
                position - start,
            )?;
        }
        let mut offset = start + u64::from(node["type"] != "doc");
        for child in children(node)? {
            check(child, offset, position)?;
            offset += node_size(child);
        }
        Ok(())
    }
    check(body, 0, position)
}
fn selection_in_valid_body(body: &Value, from: u64, to: u64) -> Result<String> {
    if from >= to || to > node_size(body) {
        return Err("Selection is outside the document".into());
    }
    fn visit(
        node: &Value,
        pos: u64,
        from: u64,
        to: u64,
        out: &mut String,
        first: &mut bool,
    ) -> Result<()> {
        let name = node["type"].as_str().unwrap();
        let end = pos + node_size(node);
        if from >= end || to <= pos {
            return Ok(());
        }
        let is_leaf = leaf(name);
        if matches!(name, "paragraph" | "heading" | "codeBlock") || block(name) && is_leaf {
            if *first {
                *first = false
            } else {
                out.push('\n');
            }
        }
        if name == "text" {
            out.push_str(&utf16_slice(
                node["text"].as_str().unwrap(),
                from.saturating_sub(pos),
                to.min(end) - pos,
            )?);
        } else if is_leaf {
            out.push('\u{fffc}');
        } else {
            let mut child_pos = pos + u64::from(name != "doc");
            for child in node["content"].as_array().into_iter().flatten() {
                visit(child, child_pos, from, to, out, first)?;
                child_pos += node_size(child);
            }
        }
        Ok(())
    }
    let mut out = String::new();
    let mut first = true;
    visit(body, 0, from, to, &mut out, &mut first)?;
    Ok(out)
}
/// Semantic text plus selected attachment references, never unselected document data.
pub(crate) fn selection_content(
    document: &Value,
    from: u64,
    to: u64,
) -> Result<(String, Vec<String>)> {
    selection_text(document, from, to)?;
    fn visit(
        node: &Value,
        pos: u64,
        from: u64,
        to: u64,
        out: &mut String,
        first: &mut bool,
        ids: &mut Vec<String>,
    ) -> Result<()> {
        let name = node["type"].as_str().unwrap();
        let end = pos + node_size(node);
        if from >= end || to <= pos {
            return Ok(());
        }
        let atom = leaf(name);
        if matches!(name, "paragraph" | "heading" | "codeBlock") || block(name) && atom {
            if *first {
                *first = false
            } else {
                out.push('\n');
            }
        }
        if name == "text" {
            out.push_str(&utf16_slice(
                node["text"].as_str().unwrap(),
                from.saturating_sub(pos),
                to.min(end) - pos,
            )?);
        } else if atom {
            match name {
                "hardBreak" => out.push('\n'),
                "workBlock" => out.push_str(&format!(
                    "[{}: {}]\n{}\n{}\n{}",
                    node["attrs"]["kind"].as_str().unwrap_or("Work"),
                    node["attrs"]["title"].as_str().unwrap_or(""),
                    node["attrs"]["text"].as_str().unwrap_or(""),
                    node["attrs"]["targetId"].as_str().unwrap_or(""),
                    node["attrs"]["html"].as_str().unwrap_or("")
                )),
                "inlineMath" => out.push_str(&format!(
                    "${}$",
                    node["attrs"]["latex"].as_str().unwrap_or("")
                )),
                "blockMath" => out.push_str(&format!(
                    "$$\n{}\n$$",
                    node["attrs"]["latex"].as_str().unwrap_or("")
                )),
                "image" | "fileAttachment" => {
                    let id = node["attrs"]["attachmentId"].as_str().unwrap();
                    if !ids.iter().any(|old| old == id) {
                        if ids.len() >= 20 {
                            return Err("Select no more than 20 attached files or images".into());
                        }
                        ids.push(id.into());
                    }
                    out.push_str(&format!(
                        "[Selected {}: {} ({id})]",
                        if name == "image" { "image" } else { "file" },
                        node["attrs"]["name"].as_str().unwrap_or("")
                    ));
                }
                _ => out.push('\u{fffc}'),
            }
        } else {
            let mut child_pos = pos + u64::from(name != "doc");
            for child in node["content"].as_array().into_iter().flatten() {
                visit(child, child_pos, from, to, out, first, ids)?;
                child_pos += node_size(child);
            }
        }
        Ok(())
    }
    let mut out = String::new();
    let mut ids = Vec::new();
    visit(
        &document["body"],
        0,
        from,
        to,
        &mut out,
        &mut true,
        &mut ids,
    )?;
    Ok((out, ids))
}
fn escape_text(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if matches!(
            c,
            '\\' | '`'
                | '*'
                | '_'
                | '['
                | ']'
                | '<'
                | '>'
                | '#'
                | '!'
                | '|'
                | '~'
                | '-'
                | '+'
                | '.'
                | '('
                | ')'
                | '$'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}
fn html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
fn inline_md(node: &Value) -> String {
    let ty = node["type"].as_str().unwrap();
    let raw = node["text"].as_str().unwrap_or("");
    let marks = node["marks"].as_array();
    let code = marks.is_some_and(|m| m.iter().any(|m| m["type"] == "code"));
    let mut text = match ty {
        "text" if code => {
            let length = raw.split(|c| c != '`').map(str::len).max().unwrap_or(0) + 1;
            let fence = "`".repeat(length);
            // CommonMark removes one surrounding space only when both ends are
            // spaces and the content is not entirely spaces.
            let pad = raw.starts_with('`')
                || raw.ends_with('`')
                || (raw.starts_with(' ') && raw.ends_with(' ') && !raw.chars().all(|c| c == ' '));
            if pad {
                format!("{fence} {raw} {fence}")
            } else {
                format!("{fence}{raw}{fence}")
            }
        }
        "text" => escape_text(raw),
        "hardBreak" => "  \n".into(),
        "inlineMath" => format!("${}$", node["attrs"]["latex"].as_str().unwrap_or("")),
        _ => String::new(),
    };
    for mark in marks.into_iter().flatten().rev() {
        text = match mark["type"].as_str().unwrap_or("") {
            "bold" => format!("**{text}**"),
            "italic" => format!("*{text}*"),
            "strike" => format!("~~{text}~~"),
            "underline" => format!("<u>{text}</u>"),
            "link" => format!(
                "[{text}]({})",
                mark["attrs"]["href"]
                    .as_str()
                    .unwrap()
                    .replace('(', "%28")
                    .replace(')', "%29")
            ),
            _ => text,
        };
    }
    text
}
fn html_node(node: &Value) -> String {
    let name = node["type"].as_str().unwrap();
    let contents = node["content"]
        .as_array()
        .into_iter()
        .flatten()
        .map(html_node)
        .collect::<String>();
    match name {
        "text" => {
            let mut text = html(node["text"].as_str().unwrap());
            for mark in node["marks"].as_array().into_iter().flatten().rev() {
                text = match mark["type"].as_str().unwrap() {
                    "bold" => format!("<strong>{text}</strong>"),
                    "italic" => format!("<em>{text}</em>"),
                    "strike" => format!("<s>{text}</s>"),
                    "underline" => format!("<u>{text}</u>"),
                    "code" => format!("<code>{text}</code>"),
                    "link" => format!(
                        "<a href=\"{}\">{text}</a>",
                        html(mark["attrs"]["href"].as_str().unwrap())
                    ),
                    _ => text,
                };
            }
            text
        }
        "paragraph" => format!("<p>{contents}</p>"),
        "heading" => {
            let level = node["attrs"]["level"].as_u64().unwrap_or(1);
            format!("<h{level}>{contents}</h{level}>")
        }
        "hardBreak" => "<br>".into(),
        "horizontalRule" => "<hr>".into(),
        "blockquote" => format!("<blockquote>{contents}</blockquote>"),
        "codeBlock" => format!("<pre><code>{contents}</code></pre>"),
        "bulletList" | "taskList" => format!("<ul>{contents}</ul>"),
        "orderedList" => format!(
            "<ol start=\"{}\">{contents}</ol>",
            node["attrs"]["start"].as_u64().unwrap_or(1)
        ),
        "listItem" | "taskItem" => format!(
            "<li>{}{contents}</li>",
            if name == "taskItem" {
                if node["attrs"]["checked"] == true {
                    "[x] "
                } else {
                    "[ ] "
                }
            } else {
                ""
            }
        ),
        "table" => format!("<table>{contents}</table>"),
        "tableRow" => format!("<tr>{contents}</tr>"),
        "tableCell" | "tableHeader" => {
            let tag = if name == "tableCell" { "td" } else { "th" };
            let span = node["attrs"]["colspan"].as_u64().unwrap_or(1);
            let rows = node["attrs"]["rowspan"].as_u64().unwrap_or(1);
            format!("<{tag} colspan=\"{span}\" rowspan=\"{rows}\">{contents}</{tag}>")
        }
        "inlineMath" | "blockMath" => html(&format!(
            "${}$",
            node["attrs"]["latex"].as_str().unwrap_or("")
        )),
        "image" | "fileAttachment" => html(&format!(
            "[{}: {}]",
            node["attrs"]["name"].as_str().unwrap_or("Attachment"),
            node["attrs"]["attachmentId"].as_str().unwrap()
        )),
        "workBlock" => html(&format!(
            "{}\n{}\n{}\n{}",
            node["attrs"]["title"].as_str().unwrap_or(""),
            node["attrs"]["text"].as_str().unwrap_or(""),
            node["attrs"]["targetId"].as_str().unwrap_or(""),
            node["attrs"]["html"].as_str().unwrap_or("")
        )),
        _ => contents,
    }
}
fn markdown_node(node: &Value) -> String {
    let name = node["type"].as_str().unwrap();
    let items = node["content"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    match name {
        "workBlock" => format!(
            "```mcodex-block\n{}\n```\n\n",
            serde_json::to_string(&node["attrs"]).unwrap()
        ),
        "doc" => items.iter().map(markdown_node).collect(),
        "paragraph" => format!("{}\n\n", items.iter().map(inline_md).collect::<String>()),
        "heading" => format!(
            "{} {}\n\n",
            "#".repeat(node["attrs"]["level"].as_u64().unwrap_or(1) as usize),
            items.iter().map(inline_md).collect::<String>()
        ),
        "blockquote" => format!(
            "{}\n\n",
            items
                .iter()
                .map(markdown_node)
                .collect::<String>()
                .trim_end_matches('\n')
                .lines()
                .map(|line| format!("> {line}"))
                .collect::<Vec<_>>()
                .join("\n")
        ),
        "horizontalRule" => "---\n\n".into(),
        "codeBlock" => {
            let content = items
                .iter()
                .filter_map(|n| n["text"].as_str())
                .collect::<String>();
            let language = node["attrs"]["language"].as_str().unwrap_or("");
            let fence = if language.contains('`') { '~' } else { '`' };
            let run = content
                .split(|c| c != fence)
                .map(str::len)
                .max()
                .unwrap_or(0)
                + 1;
            let marker = fence.to_string().repeat(run.max(3));
            format!(
                "{marker}{language}\n{content}{}{marker}\n\n",
                if content.ends_with('\n') { "" } else { "\n" }
            )
        }
        "bulletList" | "orderedList" | "taskList" => {
            let mut out = String::new();
            for (index, item) in items.iter().enumerate() {
                let prefix = if name == "orderedList" {
                    format!(
                        "{}. ",
                        node["attrs"]["start"].as_u64().unwrap_or(1) + index as u64
                    )
                } else if name == "taskList" {
                    if item["attrs"]["checked"] == true {
                        "- [x] ".into()
                    } else {
                        "- [ ] ".into()
                    }
                } else {
                    "- ".into()
                };
                let child = markdown_node(item);
                for (line_index, line) in child.trim_end_matches('\n').split('\n').enumerate() {
                    out.push_str(if line_index == 0 { &prefix } else { "    " });
                    out.push_str(line);
                    out.push('\n');
                }
            }
            out.push('\n');
            out
        }
        "listItem" | "taskItem" | "tableCell" | "tableHeader" => {
            items.iter().map(markdown_node).collect()
        }
        "table" => format!("{}\n\n", html_node(node)),
        "image" | "fileAttachment" => {
            let label = if name == "image" {
                node["attrs"]["alt"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .or(node["attrs"]["name"].as_str())
                    .unwrap_or("")
            } else {
                node["attrs"]["name"].as_str().unwrap_or("")
            };
            format!(
                "{}[{}](attachment:{})\n\n",
                if name == "image" { "!" } else { "" },
                escape_text(label),
                node["attrs"]["attachmentId"].as_str().unwrap()
            )
        }
        "blockMath" => format!(
            "$$\n{}\n$$\n\n",
            node["attrs"]["latex"].as_str().unwrap_or("")
        ),
        _ => inline_md(node),
    }
}
pub(crate) fn markdown(body: &Value) -> Result<String> {
    validate_body(body)?;
    let text = markdown_node(body);
    if text.len() + serde_json::to_vec(body).unwrap().len() > MAX_CURRENT {
        return Err("Current document body and Markdown projection exceed 8 MiB".into());
    }
    Ok(text)
}

fn snapshot(document: &Value, reason: &str, restored: Option<u64>) -> Value {
    json!({"revision":document["revision"],"savedAt":document["updatedAt"],"reason":reason,"restoredFrom":restored,"title":document["title"],"kind":document["kind"],"format":document["format"],"schemaVersion":document["schemaVersion"],"body":document["body"],"content":document["content"],"projectId":document["projectId"],"parentId":document["parentId"],"commentCount":document["_comments"].as_array().map_or(0,Vec::len)})
}
fn check_current(document: &Value) -> Result<()> {
    if !safe_string(string(document, "title")?, 1024)
        || document["title"].as_str().unwrap().trim().is_empty()
    {
        return Err("Document title must contain 1 to 1024 valid UTF-8 bytes".into());
    }
    if !matches!(document["kind"].as_str(), Some("document" | "code")) {
        return Err("kind must be document or code".into());
    }
    if !matches!(document["schemaVersion"].as_u64(), Some(1 | 2)) {
        return Err("Unsupported document schema version".into());
    }
    let content = document["content"]
        .as_str()
        .ok_or("Document content must be a string")?;
    if content.len() > MAX_CURRENT {
        return Err("Document content exceeds 8 MiB".into());
    }
    match document["format"].as_str() {
        Some("markdown") => {
            if !document["body"].is_null() {
                return Err("Markdown documents cannot have a rich body".into());
            }
        }
        Some("richtext") => {
            fn has_work(node: &Value) -> bool {
                node["type"] == "workBlock"
                    || node["content"]
                        .as_array()
                        .is_some_and(|items| items.iter().any(has_work))
            }
            if document["schemaVersion"] == 1 && has_work(&document["body"]) {
                return Err("Local Work blocks require schemaVersion 2".into());
            }
            if document["kind"] != "document" {
                return Err("Code documents remain plain source text".into());
            }
            let projected = markdown(&document["body"])?;
            if projected != content {
                return Err("Stored Markdown does not match the structured document body".into());
            }
        }
        _ => return Err("format must be markdown or richtext".into()),
    }
    if !document["sourceMarkdown"].is_null()
        && document["sourceMarkdown"]
            .as_str()
            .is_none_or(|s| s.len() > MAX_CURRENT)
    {
        return Err("Original Markdown exceeds its 8 MiB bound or is invalid".into());
    }
    if document["revision"].as_u64().is_none_or(|n| n == 0) {
        return Err("Invalid document revision".into());
    }
    if !document["projectId"].is_null() && document["projectId"].as_str().is_none_or(str::is_empty)
    {
        return Err("Invalid document project reference".into());
    }
    if !document["parentId"].is_null() && document["parentId"].as_str().is_none_or(str::is_empty) {
        return Err("Invalid page parent reference".into());
    }
    Ok(())
}
fn normalize(raw: &Value) -> Result<Value> {
    let mut doc = raw
        .as_object()
        .ok_or("Invalid stored document record")?
        .clone();
    if !doc.contains_key("parentId") {
        doc.insert("parentId".into(), Value::Null);
    }
    if !doc.contains_key("format") {
        doc.insert("format".into(), json!("markdown"));
    }
    if !doc.contains_key("body") {
        doc.insert("body".into(), Value::Null);
    }
    if !doc.contains_key("schemaVersion") {
        doc.insert("schemaVersion".into(), json!(1));
    }
    if !doc.contains_key("revision") {
        doc.insert("revision".into(), json!(1));
    }
    if !doc.contains_key("sourceMarkdown") {
        doc.insert(
            "sourceMarkdown".into(),
            doc.get("content").cloned().unwrap_or_else(|| json!("")),
        );
    }
    if !doc.contains_key("_comments") {
        doc.insert("_comments".into(), json!([]));
    }
    if !doc.contains_key("_historyPruned") {
        doc.insert("_historyPruned".into(), json!(0));
    }
    let mut doc = Value::Object(doc);
    check_current(&doc)?;
    if !doc["_comments"].is_array() || doc["_comments"].as_array().unwrap().len() > MAX_COMMENTS {
        return Err("Invalid stored comment collection".into());
    }
    let mut seen = HashSet::new();
    for comment in doc["_comments"].as_array().unwrap() {
        let id = string(comment, "id")?;
        if !seen.insert(id) {
            return Err("Duplicate stored comment id".into());
        }
        let body = string(comment, "body")?;
        if !safe_string(body, MAX_COMMENT)
            || !comment["resolved"].is_boolean()
            || !comment["orphaned"].is_boolean()
        {
            return Err("Invalid stored comment".into());
        }
        if comment["author"] != json!({"name":"You","source":"local"})
            || !comment["createdAt"].is_u64()
            || !comment["updatedAt"].is_u64()
        {
            return Err("Invalid local comment attribution or timestamp".into());
        }
        if !comment["anchor"].is_null() {
            let a = &comment["anchor"];
            fields(a, &["revision", "from", "to", "quote"])?;
            let revision = integer(a, "revision")?;
            let from = integer(a, "from")?;
            let to = integer(a, "to")?;
            if revision == 0
                || revision > doc["revision"].as_u64().unwrap()
                || from >= to
                || to > (MAX_CURRENT + MAX_NODES * 2) as u64
                || a["quote"]
                    .as_str()
                    .is_none_or(|s| !safe_string(s, MAX_COMMENT))
            {
                return Err("Invalid stored comment anchor".into());
            }
            if comment["orphaned"] == false
                && (doc["format"] != "richtext"
                    || selection_in_valid_body(&doc["body"], from, to)? != a["quote"])
            {
                return Err(
                    "Stored comment quote does not match its current document range".into(),
                );
            }
        }
    }
    for comment in doc["_comments"].as_array().unwrap() {
        if !comment["parentId"].is_null() {
            let id = string(comment, "parentId")?;
            let parent = doc["_comments"]
                .as_array()
                .unwrap()
                .iter()
                .find(|c| c["id"] == id)
                .ok_or("Stored reply has no parent comment")?;
            if !parent["parentId"].is_null()
                || !comment["anchor"].is_null()
                || parent["resolved"] != comment["resolved"]
            {
                return Err("Invalid stored comment thread".into());
            }
        }
    }
    if doc.get("_history").is_none() {
        doc["_history"] = json!([snapshot(&doc, "legacy_snapshot", None)]);
    }
    let history = doc["_history"]
        .as_array()
        .filter(|h| !h.is_empty() && h.len() <= MAX_VERSIONS)
        .ok_or("Invalid stored document history")?;
    let mut previous = 0;
    for version in history {
        let revision = integer(version, "revision")?;
        if revision <= previous || revision > doc["revision"].as_u64().unwrap() {
            return Err("Invalid history revision order".into());
        }
        check_current(version)?;
        if !version["savedAt"].is_u64()
            || version["commentCount"]
                .as_u64()
                .is_none_or(|n| n > MAX_COMMENTS as u64)
            || !safe_string(string(version, "reason")?, 128)
            || !version["restoredFrom"].is_null()
                && version["restoredFrom"]
                    .as_u64()
                    .is_none_or(|n| n == 0 || n >= revision)
        {
            return Err("Invalid stored history metadata".into());
        }
        previous = revision;
    }
    let latest = history.last().unwrap();
    if previous != doc["revision"].as_u64().unwrap()
        || ["title", "kind", "format", "body", "content", "projectId"]
            .iter()
            .any(|key| latest[*key] != doc[*key])
        || !doc["_historyPruned"].is_u64()
    {
        return Err("Current document does not match its latest history snapshot".into());
    }
    if serde_json::to_vec(history).unwrap().len() > MAX_HISTORY {
        return Err("Stored history exceeds 32 MiB".into());
    }
    Ok(doc)
}
pub(crate) fn validate_collection(collection: &Value) -> Result<()> {
    for (id, raw) in collection
        .as_object()
        .ok_or("Documents must be an object")?
    {
        let doc = normalize(raw)?;
        if doc["id"].as_str() != Some(id) {
            return Err("Document id does not match its storage key".into());
        }
        let mut parent = doc["parentId"].as_str();
        let mut seen = HashSet::from([id.as_str()]);
        while let Some(ancestor) = parent {
            if !seen.insert(ancestor) || seen.len() > 65 {
                return Err("Page hierarchy contains a cycle or exceeds 64 levels".into());
            }
            let record = collection
                .get(ancestor)
                .ok_or("Page parent does not exist")?;
            parent = record["parentId"].as_str();
        }
    }
    Ok(())
}
fn retention(document: &Value) -> Value {
    let history = document["_history"].as_array().unwrap();
    json!({"count":history.len(),"retainedFromRevision":history.first().map(|v|v["revision"].clone()),"prunedCount":document["_historyPruned"],"maxVersions":MAX_VERSIONS,"maxBytes":MAX_HISTORY})
}
fn public(document: &Value) -> Value {
    let mut value = document.clone();
    let object = value.as_object_mut().unwrap();
    object.retain(|key, _| !key.starts_with('_'));
    object.insert(
        "commentCount".into(),
        json!(document["_comments"].as_array().map_or(0, Vec::len)),
    );
    object.insert("history".into(), retention(document));
    value
}
pub(crate) fn summary(raw: &Value) -> Result<Value> {
    let doc = normalize(raw)?;
    let mut result = public(&doc);
    let content = doc["content"].as_str().unwrap();
    let excerpt = content.chars().take(512).collect::<String>();
    result["contentTruncated"] = json!(excerpt.len() < content.len());
    result["content"] = json!(excerpt);
    result.as_object_mut().unwrap().remove("body");
    result.as_object_mut().unwrap().remove("sourceMarkdown");
    Ok(result)
}
pub(crate) fn summaries(collection: &Value) -> Result<Vec<Value>> {
    let mut items = collection
        .as_object()
        .ok_or("Documents must be an object")?
        .values()
        .map(summary)
        .collect::<Result<Vec<_>>>()?;
    items.sort_by(|a, b| {
        b["updatedAt"]
            .as_u64()
            .cmp(&a["updatedAt"].as_u64())
            .then(a["id"].as_str().cmp(&b["id"].as_str()))
    });
    Ok(items)
}
fn expected(document: &Value, p: &Value, required: bool) -> Result<Option<Value>> {
    let Some(value) = p.get("expectedRevision") else {
        if required {
            return Err(
                "expectedRevision is required; read the current document before saving".into(),
            );
        }
        return Ok(None);
    };
    let requested = value
        .as_u64()
        .filter(|n| *n > 0)
        .ok_or("expectedRevision must be a positive integer")?;
    let actual = document["revision"].as_u64().unwrap();
    if requested != actual {
        return Ok(Some(
            json!({"conflict":{"expectedRevision":requested,"actualRevision":actual},"document":public(document)}),
        ));
    }
    Ok(None)
}
fn project(document: &mut Value, p: &Value, state: &Value) -> Result<()> {
    if let Some(project) = p.get("projectId") {
        if !project.is_null() {
            let id = project
                .as_str()
                .filter(|s| !s.is_empty())
                .ok_or("projectId must be a string or null")?;
            if state["projects"].get(id).is_none() {
                return Err("Project not found".into());
            }
        }
        document["projectId"] = project.clone();
    }
    Ok(())
}
fn patch(document: &mut Value, p: &Value, state: &Value) -> Result<()> {
    if let Some(version) = p.get("schemaVersion") {
        if document["schemaVersion"] == 2 && version == 1 {
            return Err(
                "This document requires editor schema 2; update the client before saving".into(),
            );
        }
        document["schemaVersion"] = version.clone();
    }
    if let Some(parent) = p.get("parentId") {
        if !parent.is_null() {
            let id = parent
                .as_str()
                .filter(|id| !id.is_empty())
                .ok_or("parentId must be a page id or null")?;
            let mut next = Some(id);
            let mut seen = HashSet::new();
            while let Some(id) = next {
                if Some(id) == document["id"].as_str() || !seen.insert(id) || seen.len() > 64 {
                    return Err("A page cannot be moved under itself or its descendants".into());
                }
                let target = state["documents"].get(id).ok_or("Parent page not found")?;
                next = target["parentId"].as_str();
            }
        }
        document["parentId"] = parent.clone();
    }
    if let Some(title) = p.get("title") {
        let title = title
            .as_str()
            .filter(|s| !s.trim().is_empty() && safe_string(s, 1024))
            .ok_or("Document title must contain 1 to 1024 valid bytes")?;
        document["title"] = json!(title.trim());
    }
    if let Some(kind) = p.get("kind") {
        if !matches!(kind.as_str(), Some("document" | "code")) {
            return Err("kind must be document or code".into());
        }
        document["kind"] = kind.clone();
    }
    if let Some(format) = p.get("format") {
        if !matches!(format.as_str(), Some("markdown" | "richtext")) {
            return Err("format must be markdown or richtext".into());
        }
        if document["format"] == "richtext" && format == "markdown" {
            return Err("Use restore or create a separate Markdown document; rich data cannot be silently discarded".into());
        }
        document["format"] = format.clone();
    }
    if p.get("body").is_some() {
        if document["format"] != "richtext" {
            return Err("body requires format richtext".into());
        }
        if p.get("content").is_some() {
            return Err("Rich content is derived from body; do not also send content".into());
        }
        document["body"] = p["body"].clone();
        document["content"] = json!(markdown(&document["body"])?);
    }
    if let Some(content) = p.get("content") {
        if document["format"] == "richtext" {
            return Err(
                "Rich content is derived from body and cannot be overwritten independently".into(),
            );
        }
        let content = content
            .as_str()
            .filter(|s| s.len() <= MAX_CURRENT)
            .ok_or("Markdown content must be valid text of at most 8 MiB")?;
        document["content"] = json!(content);
    }
    if document["format"] == "richtext" && document["body"].is_null() {
        return Err("A richtext document requires a structured body".into());
    }
    project(document, p, state)?;
    check_current(document)
}
fn save_next(
    document: &mut Value,
    before: &Value,
    reason: &str,
    restored: Option<u64>,
) -> Result<()> {
    check_current(document)?;
    let changed_body = document["body"] != before["body"]
        || document["format"] != before["format"]
        || document["content"] != before["content"];
    if changed_body {
        for comment in document["_comments"].as_array_mut().unwrap() {
            if !comment["anchor"].is_null() {
                comment["orphaned"] = json!(true);
            }
        }
    }
    document["revision"] = json!(before["revision"]
        .as_u64()
        .unwrap()
        .checked_add(1)
        .ok_or("Document revision exhausted")?);
    document["updatedAt"] =
        json!(now().max(before["updatedAt"].as_u64().unwrap_or(0).saturating_add(1)));
    let version = snapshot(document, reason, restored);
    let history = document["_history"].as_array_mut().unwrap();
    history.push(version);
    let mut bytes = history
        .iter()
        .map(|item| serde_json::to_vec(item).unwrap().len() + 1)
        .sum::<usize>()
        + 2;
    let mut pruned = 0u64;
    while history.len() > MAX_VERSIONS || bytes > MAX_HISTORY {
        if history.len() <= 1 {
            return Err("Current history snapshot exceeds its 32 MiB budget".into());
        }
        let removed = history.remove(0);
        bytes = bytes.saturating_sub(serde_json::to_vec(&removed).unwrap().len() + 1);
        pruned += 1;
    }
    document["_historyPruned"] = json!(document["_historyPruned"]
        .as_u64()
        .unwrap_or(0)
        .saturating_add(pruned));
    Ok(())
}
pub(crate) fn detach_project(state: &mut Value, project_id: &str) -> Result<()> {
    let ids = state["documents"]
        .as_object()
        .unwrap()
        .iter()
        .filter(|(_, doc)| doc["projectId"].as_str() == Some(project_id))
        .map(|(id, _)| id.clone())
        .collect::<Vec<_>>();
    for id in ids {
        let before = normalize(&state["documents"][&id])?;
        let mut doc = before.clone();
        doc["projectId"] = Value::Null;
        save_next(&mut doc, &before, "project_deleted", None)?;
        state["documents"][&id] = doc;
    }
    Ok(())
}
fn limit(p: &Value, key: &str, default: u64, max: u64) -> Result<usize> {
    match p.get(key) {
        None => Ok(default as usize),
        Some(v) => v
            .as_u64()
            .filter(|n| *n > 0 && *n <= max)
            .map(|n| n as usize)
            .ok_or_else(|| format!("{key} must be between 1 and {max}")),
    }
}
fn anchor(document: &Value, p: &Value) -> Result<Value> {
    let Some(value) = p.get("anchor").filter(|v| !v.is_null()) else {
        return Ok(Value::Null);
    };
    fields(value, &["revision", "from", "to", "quote"])?;
    if integer(value, "revision")? != document["revision"].as_u64().unwrap() {
        return Err("Comment anchor must use the current document revision".into());
    }
    let from = integer(value, "from")?;
    let to = integer(value, "to")?;
    if from >= to {
        return Err("Comment anchor must select a non-empty range".into());
    }
    let selected = selection_text(document, from, to)?;
    if value.get("quote").is_some() && value["quote"].as_str() != Some(selected.as_str()) {
        return Err("Comment quote does not match its document range".into());
    }
    if selected.len() > MAX_COMMENT {
        return Err("Comment quote exceeds 16 KiB; select a smaller range".into());
    }
    Ok(json!({"revision":document["revision"],"from":from,"to":to,"quote":selected}))
}

/// Return None for non-document methods; changed is the caller's atomic-save flag.
pub(crate) fn handle(state: &mut Value, method: &str, p: &Value) -> Option<Result<(Value, bool)>> {
    if !METHODS.contains(&method) {
        return None;
    }
    Some(dispatch(state, method, p))
}
fn dispatch(state: &mut Value, method: &str, p: &Value) -> Result<(Value, bool)> {
    if method == "desktop/document/create" {
        fields(
            p,
            &[
                "title",
                "content",
                "kind",
                "projectId",
                "format",
                "body",
                "schemaVersion",
                "parentId",
            ],
        )?;
        if p.get("schemaVersion")
            .is_some_and(|v| !matches!(v.as_u64(), Some(1 | 2)))
        {
            return Err("Unsupported document schema version".into());
        }
        let id = new_id("document");
        let time = now();
        let mut doc = json!({"id":id,"title":"Untitled","kind":"document","format":"markdown","schemaVersion":1,"body":null,"content":"","sourceMarkdown":null,"projectId":null,"parentId":null,"createdAt":time,"updatedAt":time,"revision":1,"_comments":[],"_history":[],"_historyPruned":0});
        patch(&mut doc, p, state)?;
        if p.get("projectId").is_none() {
            if let Some(parent) = doc["parentId"].as_str() {
                doc["projectId"] = state["documents"][parent]["projectId"].clone();
            }
        }
        if doc["format"] == "markdown" {
            doc["sourceMarkdown"] = doc["content"].clone();
        }
        doc["_history"] = json!([snapshot(&doc, "create", None)]);
        state["documents"][&id] = doc.clone();
        return Ok((json!({"document":public(&doc)}), true));
    }
    if method == "desktop/document/list" {
        fields(
            p,
            &["kind", "projectId", "parentId", "search", "mode", "limit"],
        )?;
        let search = p
            .get("search")
            .map(|s| s.as_str().ok_or("search must be a string"))
            .transpose()?
            .unwrap_or("")
            .to_lowercase();
        let mut found = Vec::new();
        if p.get("kind")
            .is_some_and(|v| !matches!(v.as_str(), Some("document" | "code")))
        {
            return Err("kind must be document or code".into());
        }
        if p.get("projectId")
            .is_some_and(|v| !v.is_null() && !v.is_string())
        {
            return Err("projectId must be a string or null".into());
        }
        if p.get("mode")
            .is_some_and(|v| !matches!(v.as_str(), Some("chat" | "work" | "codex")))
        {
            return Err("mode must be chat, work or codex".into());
        }
        for raw in state["documents"].as_object().unwrap().values() {
            let doc = normalize(raw)?;
            if ["kind", "projectId", "parentId", "mode"]
                .iter()
                .any(|key| p.get(*key).is_some_and(|value| doc[*key] != *value))
            {
                continue;
            }
            if !search.is_empty()
                && !format!(
                    "{}\n{}",
                    doc["title"].as_str().unwrap(),
                    doc["content"].as_str().unwrap()
                )
                .to_lowercase()
                .contains(&search)
            {
                continue;
            }
            found.push(summary(&doc)?);
        }
        found.sort_by(|a, b| {
            b["updatedAt"]
                .as_u64()
                .cmp(&a["updatedAt"].as_u64())
                .then(a["id"].as_str().cmp(&b["id"].as_str()))
        });
        if p.get("limit").is_some() {
            found.truncate(limit(p, "limit", 100, 1000)?);
        }
        return Ok((json!({"items":found}), false));
    }
    let id = string(p, "id")?;
    let before = normalize(state["documents"].get(id).ok_or("Document not found")?)?;
    let mut doc = before.clone();
    match method {
        "desktop/document/read" => {
            fields(p, &["id"])?;
            return Ok((json!({"document":public(&doc)}), false));
        }
        "desktop/document/history/list" => {
            fields(p, &["id", "limit", "beforeRevision"])?;
            let count = limit(p, "limit", 50, 100)?;
            let before_revision = p
                .get("beforeRevision")
                .map(|v| v.as_u64().ok_or("beforeRevision must be an integer"))
                .transpose()?;
            let items=doc["_history"].as_array().unwrap().iter().rev().filter(|v|before_revision.is_none_or(|before|v["revision"].as_u64().unwrap()<before)).take(count).map(|v|json!({"revision":v["revision"],"savedAt":v["savedAt"],"reason":v["reason"],"restoredFrom":v["restoredFrom"],"title":v["title"],"format":v["format"],"commentCount":v["commentCount"]})).collect::<Vec<_>>();
            return Ok((json!({"items":items,"history":retention(&doc)}), false));
        }
        "desktop/document/history/read" | "desktop/document/export" => {
            fields(
                p,
                if method.ends_with("export") {
                    &["id", "revision", "format"]
                } else {
                    &["id", "revision"]
                },
            )?;
            if method.ends_with("read") && p.get("revision").is_none() {
                return Err("revision is required to read a document version".into());
            }
            let version = if p.get("revision").is_some() {
                let revision = integer(p, "revision")?;
                doc["_history"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|v| v["revision"] == revision)
                    .cloned()
                    .ok_or("Document version is unavailable or was pruned")?
            } else {
                snapshot(&doc, "current", None)
            };
            if method.ends_with("read") {
                return Ok((
                    json!({"version":version,"sourceMarkdown":doc["sourceMarkdown"],"documentId":id}),
                    false,
                ));
            }
            let format = string(p, "format")?;
            let filename = version["title"]
                .as_str()
                .unwrap()
                .chars()
                .take(120)
                .map(|c| {
                    if c == '/' || c == '\\' || c.is_control() {
                        '_'
                    } else {
                        c
                    }
                })
                .collect::<String>();
            if format == "docx" {
                use base64::{engine::general_purpose::STANDARD, Engine};
                let exported = crate::document_export::docx(&version)?;
                return Ok((
                    json!({"filename":format!("{filename}.docx"),"mimeType":"application/vnd.openxmlformats-officedocument.wordprocessingml.document","dataBase64":STANDARD.encode(exported.bytes),"warnings":exported.warnings}),
                    false,
                ));
            }
            let (mime,content,extension)=match format{"markdown"=>("text/markdown;charset=utf-8",version["content"].as_str().unwrap().into(),"md"),"json"=>("application/json;charset=utf-8",serde_json::to_string_pretty(&json!({"documentId":id,"sourceMarkdown":doc["sourceMarkdown"],"version":version})).unwrap(),"json"),_=>return Err("Document export format must be markdown, json or docx".into())};
            return Ok((
                json!({"filename":format!("{filename}.{extension}"),"mimeType":mime,"content":content}),
                false,
            ));
        }
        "desktop/document/comments/list" => {
            fields(p, &["id"])?;
            return Ok((
                json!({"items":doc["_comments"],"revision":doc["revision"]}),
                false,
            ));
        }
        "desktop/document/update" => {
            fields(
                p,
                &[
                    "id",
                    "expectedRevision",
                    "title",
                    "content",
                    "kind",
                    "projectId",
                    "format",
                    "body",
                    "schemaVersion",
                    "parentId",
                ],
            )?;
            if p.get("schemaVersion")
                .is_some_and(|v| !matches!(v.as_u64(), Some(1 | 2)))
            {
                return Err("Unsupported document schema version".into());
            }
            let required = doc["format"] == "richtext"
                || p["format"] == "richtext"
                || p.get("body").is_some()
                || p.get("parentId").is_some();
            if let Some(conflict) = expected(&doc, p, required)? {
                return Ok((conflict, false));
            }
            patch(&mut doc, p, state)?;
            if doc == before {
                return Ok((json!({"document":public(&doc)}), false));
            }
            save_next(&mut doc, &before, "edit", None)?;
        }
        "desktop/document/delete" => {
            fields(p, &["id", "expectedRevision"])?;
            if let Some(conflict) = expected(&doc, p, doc["format"] == "richtext")? {
                return Ok((conflict, false));
            }
            let parent = doc["parentId"].clone();
            for child in state["documents"].as_object_mut().unwrap().values_mut() {
                if child["parentId"].as_str() == Some(id) {
                    *child = normalize(child)?;
                    let before = child.clone();
                    child["parentId"] = parent.clone();
                    save_next(child, &before, "parent_deleted", None)?;
                }
            }
            state["documents"].as_object_mut().unwrap().remove(id);
            return Ok((json!({"id":id,"deleted":true}), true));
        }
        "desktop/document/restore" => {
            fields(p, &["id", "revision", "expectedRevision"])?;
            if let Some(conflict) = expected(&doc, p, true)? {
                return Ok((conflict, false));
            }
            let revision = integer(p, "revision")?;
            let target = doc["_history"]
                .as_array()
                .unwrap()
                .iter()
                .find(|v| v["revision"] == revision)
                .cloned()
                .ok_or("Document version is unavailable or was pruned")?;
            for key in [
                "title",
                "kind",
                "format",
                "schemaVersion",
                "body",
                "content",
            ] {
                doc[key] = target[key].clone();
            }
            save_next(&mut doc, &before, "restore", Some(revision))?;
        }
        "desktop/document/comments/create" => {
            fields(p, &["id", "expectedRevision", "body", "anchor", "parentId"])?;
            if let Some(conflict) = expected(&doc, p, true)? {
                return Ok((conflict, false));
            }
            if doc["_comments"].as_array().unwrap().len() >= MAX_COMMENTS {
                return Err("A document can contain up to 1000 local comments".into());
            }
            let body = string(p, "body")?;
            if !safe_string(body, MAX_COMMENT) || body.trim().is_empty() {
                return Err("Comment text must contain 1 to 16384 valid bytes".into());
            }
            let parent = p
                .get("parentId")
                .filter(|v| !v.is_null())
                .map(|v| v.as_str().ok_or("parentId must be a string or null"))
                .transpose()?;
            if let Some(parent) = parent {
                let root = doc["_comments"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|c| c["id"].as_str() == Some(parent))
                    .ok_or("Parent comment not found")?;
                if !root["parentId"].is_null() || root["resolved"] == true {
                    return Err("Reply to an unresolved root comment".into());
                }
                if !p["anchor"].is_null() {
                    return Err("Replies inherit their root comment anchor".into());
                }
            }
            let comment = json!({"id":new_id("comment"),"parentId":parent,"body":body,"author":{"name":"You","source":"local"},"anchor":if parent.is_some(){Value::Null}else{anchor(&doc,p)?},"resolved":false,"orphaned":false,"createdAt":now(),"updatedAt":now()});
            doc["_comments"]
                .as_array_mut()
                .unwrap()
                .push(comment.clone());
            save_next(&mut doc, &before, "comment_created", None)?;
            state["documents"][id] = doc.clone();
            return Ok((json!({"document":public(&doc),"comment":comment}), true));
        }
        "desktop/document/comments/update" | "desktop/document/comments/delete" => {
            fields(
                p,
                if method.ends_with("delete") {
                    &["id", "commentId", "expectedRevision"]
                } else {
                    &["id", "commentId", "expectedRevision", "body", "resolved"]
                },
            )?;
            if let Some(conflict) = expected(&doc, p, true)? {
                return Ok((conflict, false));
            }
            let comment_id = string(p, "commentId")?;
            let position = doc["_comments"]
                .as_array()
                .unwrap()
                .iter()
                .position(|c| c["id"].as_str() == Some(comment_id))
                .ok_or("Comment not found")?;
            if method.ends_with("delete") {
                doc["_comments"].as_array_mut().unwrap().retain(|c| {
                    c["id"].as_str() != Some(comment_id)
                        && c["parentId"].as_str() != Some(comment_id)
                });
                save_next(&mut doc, &before, "comment_deleted", None)?;
            } else {
                if p.get("body").is_none() && p.get("resolved").is_none() {
                    return Err("Supply comment body or resolved state".into());
                }
                let comments = doc["_comments"].as_array_mut().unwrap();
                if let Some(body) = p.get("body") {
                    let body = body
                        .as_str()
                        .filter(|s| !s.trim().is_empty() && safe_string(s, MAX_COMMENT))
                        .ok_or("Invalid comment text")?;
                    comments[position]["body"] = json!(body);
                }
                if let Some(resolved) = p.get("resolved") {
                    let resolved = resolved.as_bool().ok_or("resolved must be boolean")?;
                    if !comments[position]["parentId"].is_null() {
                        return Err("Resolve the root comment thread".into());
                    }
                    for comment in comments.iter_mut().filter(|c| {
                        c["id"].as_str() == Some(comment_id)
                            || c["parentId"].as_str() == Some(comment_id)
                    }) {
                        comment["resolved"] = json!(resolved);
                    }
                }
                comments[position]["updatedAt"] = json!(now());
                save_next(&mut doc, &before, "comment_updated", None)?;
            }
        }
        _ => return Err("Unknown document method".into()),
    }
    state["documents"][id] = doc.clone();
    Ok((json!({"document":public(&doc)}), true))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> Value {
        json!({"documents":{},"projects":{}})
    }
    fn body(text: &str) -> Value {
        json!({"type":"doc","content":[{"type":"paragraph","content":[{"type":"text","text":text}]}]})
    }
    fn call(state: &mut Value, method: &str, params: Value) -> Value {
        handle(state, method, &params).unwrap().unwrap().0
    }
    fn rich(state: &mut Value, text: &str) -> Value {
        call(
            state,
            "desktop/document/create",
            json!({"format":"richtext","body":body(text)}),
        )["document"]
            .clone()
    }
    #[test]
    fn hierarchy_rejects_cycles_and_deleting_a_parent_preserves_children_with_new_revisions() {
        let mut data = state();
        let parent = rich(&mut data, "parent");
        let child = call(
            &mut data,
            "desktop/document/create",
            json!({"title":"Child","parentId":parent["id"]}),
        )["document"]
            .clone();
        let grandchild = call(
            &mut data,
            "desktop/document/create",
            json!({"title":"Grandchild","parentId":child["id"]}),
        )["document"]
            .clone();
        err_unchanged(
            &mut data,
            "desktop/document/update",
            json!({"id":parent["id"],"expectedRevision":1,"parentId":grandchild["id"]}),
        );
        call(
            &mut data,
            "desktop/document/delete",
            json!({"id":child["id"],"expectedRevision":1}),
        );
        let saved = call(
            &mut data,
            "desktop/document/read",
            json!({"id":grandchild["id"]}),
        )["document"]
            .clone();
        assert_eq!(saved["parentId"], parent["id"]);
        assert_eq!(saved["revision"], 2);
        validate_collection(&data["documents"]).unwrap();
        let conflict = call(
            &mut data,
            "desktop/document/update",
            json!({"id":grandchild["id"],"expectedRevision":1,"parentId":null}),
        );
        assert!(conflict["conflict"].is_object());
    }
    #[test]
    fn work_schema_is_explicit_and_insertion_positions_use_utf16_boundaries() {
        let mut data = state();
        let structured = json!({"type":"doc","content":[{"type":"workBlock","attrs":{"kind":"prompt","title":"Request","text":"中文","targetId":"","html":""}}]});
        err_unchanged(
            &mut data,
            "desktop/document/create",
            json!({"format":"richtext","body":structured}),
        );
        let doc = call(
            &mut data,
            "desktop/document/create",
            json!({"format":"richtext","schemaVersion":2,"body":structured}),
        )["document"]
            .clone();
        assert_eq!(doc["schemaVersion"], 2);
        assert!(doc["content"].as_str().unwrap().contains("mcodex-block"));
        err_unchanged(
            &mut data,
            "desktop/document/update",
            json!({"id":doc["id"],"expectedRevision":1,"schemaVersion":1}),
        );
        assert!(selection_content(&doc, 0, 1).unwrap().0.contains("中文"));
        let unicode = rich(&mut data, "A😀B");
        assert!(insertion_position(&unicode, 2).is_ok());
        assert!(insertion_position(&unicode, 3).is_err());
        assert!(insertion_position(&unicode, 999).is_err());
    }
    fn err_unchanged(state: &mut Value, method: &str, params: Value) -> String {
        let before = state.clone();
        let error = handle(state, method, &params).unwrap().unwrap_err();
        assert_eq!(
            *state, before,
            "Failed document operation mutated its transaction"
        );
        error
    }
    fn att() -> &'static str {
        "att_0123456789abcdef0123456789abcdef"
    }

    #[test]
    fn frozen_schema_is_preserved_and_projection_cannot_diverge() {
        let mut state = state();
        let structured = json!({"type":"doc","content":[
            {"type":"heading","attrs":{"level":6},"content":[{"type":"text","text":"标题","marks":[{"type":"bold"},{"type":"link","attrs":{"href":"https://example.test/a?b=1&c=2"}}]}]},
            {"type":"paragraph","content":[{"type":"text","text":"<script> unsafe #1","marks":[{"type":"underline"},{"type":"italic"},{"type":"strike"}]},{"type":"hardBreak"},{"type":"inlineMath","attrs":{"latex":"x^2"}},{"type":"text","text":"a ` b","marks":[{"type":"code"}]}]},
            {"type":"blockquote","content":[{"type":"paragraph"}]},
            {"type":"orderedList","attrs":{"start":7,"type":"a"},"content":[{"type":"listItem","content":[{"type":"paragraph","content":[{"type":"text","text":"number"}]}]}]},
            {"type":"bulletList","content":[{"type":"listItem","content":[{"type":"paragraph"}]}]},
            {"type":"taskList","content":[{"type":"taskItem","attrs":{"checked":true},"content":[{"type":"paragraph","content":[{"type":"text","text":"done"}]},{"type":"taskList","content":[{"type":"taskItem","attrs":{"checked":false},"content":[{"type":"paragraph"}]}]}]}]},
            {"type":"codeBlock","attrs":{"language":"rust"},"content":[{"type":"text","text":"```\nfn main() {}\n"}]},
            {"type":"horizontalRule"},
            {"type":"table","attrs":{"layout":"full"},"content":[{"type":"tableRow","content":[{"type":"tableHeader","attrs":{"colspan":2,"rowspan":1,"colwidth":[100,200],"align":"center"},"content":[{"type":"paragraph","content":[{"type":"text","text":"<x>"}]}]}]}]},
            {"type":"image","attrs":{"attachmentId":att(),"name":"image.png","alt":"图","title":"Title","width":240,"height":160}},
            {"type":"fileAttachment","attrs":{"attachmentId":att(),"name":"data.pdf","mimeType":"application/pdf","size":123}},
            {"type":"blockMath","attrs":{"latex":"\\int_a^b x dx"}}
        ]});
        let doc = call(
            &mut state,
            "desktop/document/create",
            json!({"title":"Rich","format":"richtext","schemaVersion":1,"body":structured}),
        )["document"]
            .clone();
        assert_eq!(doc["body"], structured);
        let md = doc["content"].as_str().unwrap();
        assert!(md.contains("###### ") && md.contains("- [x] done"));
        assert!(md.contains("````rust\n```\nfn main() {}\n````"));
        assert!(md.contains("&lt;x&gt;") && !md.contains("<script>"));
        assert!(md.contains("$x^2$") && md.contains("attachment:att_"));
        assert!(doc.get("_history").is_none() && doc.get("_comments").is_none());
        let id = doc["id"].as_str().unwrap();
        assert!(err_unchanged(
            &mut state,
            "desktop/document/update",
            json!({"id":id,"expectedRevision":1,"content":"overwrite"})
        )
        .contains("derived"));
        let exported = call(
            &mut state,
            "desktop/document/export",
            json!({"id":id,"format":"json"}),
        );
        let exported: Value = serde_json::from_str(exported["content"].as_str().unwrap()).unwrap();
        assert_eq!(exported["version"]["body"], structured);
        validate_collection(&state["documents"]).unwrap();
    }

    #[test]
    fn legacy_upgrade_preserves_exact_source_and_cas_does_not_write() {
        let mut state = state();
        let source = "  # original\r\n\n<script>legacy</script>\0\n[^unsupported]: original";
        state["documents"]["old"] = json!({"id":"old","title":"Old","kind":"document","content":source,"projectId":null,"createdAt":1,"updatedAt":1});
        let read = call(&mut state, "desktop/document/read", json!({"id":"old"}));
        assert_eq!(read["document"]["sourceMarkdown"], source);
        assert_eq!(read["document"]["revision"], 1);
        assert!(err_unchanged(
            &mut state,
            "desktop/document/update",
            json!({"id":"old","format":"richtext","body":body("new")})
        )
        .contains("expectedRevision"));
        let changed = call(
            &mut state,
            "desktop/document/update",
            json!({"id":"old","expectedRevision":1,"format":"richtext","body":body("new")}),
        );
        assert_eq!(changed["document"]["sourceMarkdown"], source);
        assert_eq!(changed["document"]["revision"], 2);
        let bytes = serde_json::to_vec(&state).unwrap();
        let stale = handle(
            &mut state,
            "desktop/document/update",
            &json!({"id":"old","expectedRevision":1,"body":body("stale")}),
        )
        .unwrap()
        .unwrap();
        assert!(!stale.1);
        assert_eq!(
            stale.0["conflict"],
            json!({"expectedRevision":1,"actualRevision":2})
        );
        assert_eq!(bytes, serde_json::to_vec(&state).unwrap());
        let old = call(
            &mut state,
            "desktop/document/history/read",
            json!({"id":"old","revision":1}),
        );
        assert_eq!(old["version"]["content"], source);
        let restored = call(
            &mut state,
            "desktop/document/restore",
            json!({"id":"old","revision":1,"expectedRevision":2}),
        );
        assert_eq!(restored["document"]["content"], source);
        assert_eq!(restored["document"]["format"], "markdown");
        assert_eq!(restored["document"]["sourceMarkdown"], source);
        assert_eq!(restored["document"]["revision"], 3);
    }

    #[test]
    fn unsafe_and_invalid_schema_is_rejected_without_loss() {
        let mut state = state();
        let doc = rich(&mut state, "safe");
        let id = doc["id"].as_str().unwrap();
        let cases = [
            json!({"type":"doc","content":[]}),
            json!({"type":"doc","content":[{"type":"html","attrs":{"html":"<script>"}}]}),
            json!({"type":"doc","content":[{"type":"image","attrs":{"attachmentId":att(),"src":"https://example.test/x.png"}}]}),
            json!({"type":"doc","content":[{"type":"paragraph","attrs":{"style":"display:none"}}]}),
            json!({"type":"doc","content":[{"type":"paragraph","content":[{"type":"text","text":"bad","marks":[{"type":"link","attrs":{"href":"javascript:alert(1)"}}]}]}]}),
            json!({"type":"doc","content":[{"type":"paragraph","content":[{"type":"text","text":"bad","marks":[{"type":"code"},{"type":"bold"}]}]}]}),
            json!({"type":"doc","content":[{"type":"codeBlock","content":[{"type":"text","text":"bad","marks":[{"type":"bold"}]}]}]}),
            json!({"type":"doc","content":[{"type":"taskList","content":[{"type":"taskItem","attrs":{"checked":"yes"},"content":[{"type":"paragraph"}]}]}]}),
            json!({"type":"doc","content":[{"type":"listItem","content":[{"type":"paragraph"}]}]}),
        ];
        for invalid in cases {
            err_unchanged(
                &mut state,
                "desktop/document/update",
                json!({"id":id,"expectedRevision":1,"body":invalid}),
            );
        }
        assert!(err_unchanged(
            &mut state,
            "desktop/document/update",
            json!({"id":id,"expectedRevision":1,"format":"markdown","content":"drop body"})
        )
        .contains("discarded"));
        assert!(
            err_unchanged(&mut state, "desktop/document/delete", json!({"id":id}))
                .contains("expectedRevision")
        );
        let mut damaged = state["documents"].clone();
        damaged[id]["_history"][0]["content"] = json!("tampered projection");
        assert!(validate_collection(&damaged).is_err());
    }

    #[test]
    fn table_grid_rejects_overlap_holes_and_overflow_but_allows_rowspan() {
        let cell = |span, rows| json!({"type":"tableCell","attrs":{"colspan":span,"rowspan":rows},"content":[{"type":"paragraph"}]});
        let table = |rows| json!({"type":"doc","content":[{"type":"table","content":rows}]});
        validate_body(&table(json!([
            {"type":"tableRow","content":[cell(1,2)]},
            {"type":"tableRow","content":[]}
        ])))
        .unwrap();
        for rows in [
            json!([{"type":"tableRow","content":[cell(2,1)]},{"type":"tableRow","content":[cell(1,1)]}]),
            json!([{"type":"tableRow","content":[cell(1,2)]}]),
            json!([{"type":"tableRow","content":[cell(1,1),cell(1,2)]},{"type":"tableRow","content":[cell(2,1)]}]),
            json!([{"type":"tableRow","content":[cell(1000,1),cell(1,1)]}]),
        ] {
            assert!(validate_body(&table(rows)).is_err());
        }
    }

    #[test]
    fn selections_use_pm_utf16_positions_and_only_selected_semantic_atoms() {
        let doc = json!({"format":"richtext","body":{"type":"doc","content":[
            {"type":"paragraph","content":[{"type":"text","text":"A😀B"},{"type":"hardBreak"},{"type":"inlineMath","attrs":{"latex":"x^2"}}]},
            {"type":"image","attrs":{"attachmentId":att(),"name":"selected.png"}},
            {"type":"fileAttachment","attrs":{"attachmentId":att(),"name":"same-source.pdf"}},
            {"type":"paragraph","content":[{"type":"text","text":"C"}]}
        ]}});
        assert_eq!(node_size(&doc["body"]), 13);
        assert_eq!(
            selection_text(&doc, 1, 12).unwrap(),
            "A😀B\u{fffc}\u{fffc}\n\u{fffc}\n\u{fffc}\nC"
        );
        assert_eq!(selection_text(&doc, 2, 4).unwrap(), "😀");
        assert!(selection_text(&doc, 2, 3)
            .unwrap_err()
            .contains("surrogate"));
        assert!(selection_text(&doc, 3, 4).is_err());
        assert!(selection_text(&doc, 0, 14).is_err());
        assert!(selection_text(&doc, 1, 1).is_err());
        let (text, ids) = selection_content(&doc, 1, 7).unwrap();
        assert_eq!(text, "A😀B\n$x^2$");
        assert!(ids.is_empty());
        let (text, ids) = selection_content(&doc, 8, 10).unwrap();
        assert!(text.contains("selected.png") && text.contains("same-source.pdf"));
        assert_eq!(ids, vec![att()]);
        assert_eq!(
            selection_content(&doc, 11, 12).unwrap(),
            ("C".into(), vec![])
        );
        let empty = json!({"format":"richtext","body":{"type":"doc","content":[{"type":"paragraph","content":[{"type":"text","text":"A"}]},{"type":"paragraph"},{"type":"paragraph","content":[{"type":"text","text":"B"}]}]}});
        assert_eq!(selection_text(&empty, 0, 8).unwrap(), "A\n\nB");
    }

    #[test]
    fn local_comment_threads_cas_anchors_orphaning_and_restore_are_real() {
        let mut state = state();
        let d = rich(&mut state, "abc");
        let id = d["id"].as_str().unwrap();
        assert!(err_unchanged(&mut state,"desktop/document/comments/create",json!({"id":id,"expectedRevision":1,"body":"wrong","anchor":{"revision":1,"from":1,"to":4,"quote":"not abc"}})).contains("does not match"));
        let result = call(
            &mut state,
            "desktop/document/comments/create",
            json!({"id":id,"expectedRevision":1,"body":"Review","anchor":{"revision":1,"from":1,"to":4,"quote":"abc"}}),
        );
        let root = result["comment"]["id"].as_str().unwrap();
        assert_eq!(result["document"]["revision"], 2);
        assert_eq!(result["comment"]["anchor"]["revision"], 1);
        assert_eq!(result["comment"]["orphaned"], false);
        call(
            &mut state,
            "desktop/document/update",
            json!({"id":id,"expectedRevision":2,"title":"Renamed"}),
        );
        let reply = call(
            &mut state,
            "desktop/document/comments/create",
            json!({"id":id,"expectedRevision":3,"body":"Reply","parentId":root}),
        );
        assert_eq!(reply["comment"]["parentId"], root);
        call(
            &mut state,
            "desktop/document/comments/update",
            json!({"id":id,"commentId":root,"expectedRevision":4,"resolved":true}),
        );
        let comments = call(
            &mut state,
            "desktop/document/comments/list",
            json!({"id":id}),
        );
        assert!(comments["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["resolved"] == true && c["orphaned"] == false));
        call(
            &mut state,
            "desktop/document/update",
            json!({"id":id,"expectedRevision":5,"body":body("xyz")}),
        );
        assert_eq!(
            call(
                &mut state,
                "desktop/document/comments/list",
                json!({"id":id})
            )["items"][0]["orphaned"],
            true
        );
        let restore = call(
            &mut state,
            "desktop/document/restore",
            json!({"id":id,"revision":3,"expectedRevision":6}),
        );
        assert_eq!(restore["document"]["body"], body("abc"));
        assert_eq!(restore["document"]["commentCount"], 2);
        assert_eq!(
            call(
                &mut state,
                "desktop/document/comments/list",
                json!({"id":id})
            )["items"][0]["orphaned"],
            true
        );
        validate_collection(&state["documents"]).unwrap();
        call(
            &mut state,
            "desktop/document/comments/delete",
            json!({"id":id,"commentId":root,"expectedRevision":7}),
        );
        assert_eq!(
            call(
                &mut state,
                "desktop/document/comments/list",
                json!({"id":id})
            )["items"],
            json!([])
        );
    }

    #[test]
    fn history_pruning_is_explicit_and_search_reads_beyond_excerpt() {
        let mut state = state();
        let source = format!("{}needle at end", "中".repeat(800));
        let d = call(
            &mut state,
            "desktop/document/create",
            json!({"content":source}),
        )["document"]
            .clone();
        let id = d["id"].as_str().unwrap();
        for n in 1..=105 {
            call(
                &mut state,
                "desktop/document/update",
                json!({"id":id,"title":format!("Version {n}")}),
            );
        }
        let found = call(
            &mut state,
            "desktop/document/list",
            json!({"search":"NEEDLE AT END"}),
        );
        assert_eq!(found["items"].as_array().unwrap().len(), 1);
        assert_eq!(found["items"][0]["contentTruncated"], true);
        assert!(
            found["items"][0].get("body").is_none()
                && found["items"][0].get("sourceMarkdown").is_none()
        );
        let history = call(
            &mut state,
            "desktop/document/history/list",
            json!({"id":id,"limit":100}),
        );
        assert_eq!(history["items"].as_array().unwrap().len(), 100);
        assert_eq!(history["history"]["prunedCount"], 6);
        assert_eq!(history["history"]["retainedFromRevision"], 7);
        assert!(history["items"][0].get("content").is_none());
        assert!(err_unchanged(
            &mut state,
            "desktop/document/history/read",
            json!({"id":id,"revision":1})
        )
        .contains("pruned"));
        let current = call(&mut state, "desktop/document/read", json!({"id":id}));
        assert_eq!(current["document"]["sourceMarkdown"], source);
        assert_eq!(current["document"]["content"], source);
        validate_collection(&state["documents"]).unwrap();
    }

    #[test]
    fn bounds_and_project_detach_never_clip_or_resurrect_current_content() {
        let mut state = state();
        state["projects"]["p"] = json!({"id":"p"});
        let d = call(
            &mut state,
            "desktop/document/create",
            json!({"projectId":"p","format":"richtext","body":body("text")}),
        )["document"]
            .clone();
        let id = d["id"].as_str().unwrap();
        let large = "a".repeat(MAX_CURRENT / 2);
        assert!(err_unchanged(
            &mut state,
            "desktop/document/update",
            json!({"id":id,"expectedRevision":1,"body":body(&large)})
        )
        .contains("8 MiB"));
        let mut nested = json!({"type":"paragraph"});
        for _ in 0..65 {
            nested = json!({"type":"blockquote","content":[nested]});
        }
        assert!(validate_body(&json!({"type":"doc","content":[nested]}))
            .unwrap_err()
            .contains("64 levels"));
        state["projects"].as_object_mut().unwrap().remove("p");
        detach_project(&mut state, "p").unwrap();
        let detached = call(&mut state, "desktop/document/read", json!({"id":id}));
        assert_eq!(detached["document"]["revision"], 2);
        assert!(detached["document"]["projectId"].is_null());
        let restored = call(
            &mut state,
            "desktop/document/restore",
            json!({"id":id,"revision":1,"expectedRevision":2}),
        );
        assert!(restored["document"]["projectId"].is_null());
        assert_eq!(restored["document"]["body"], body("text"));
        validate_collection(&state["documents"]).unwrap();
    }
}
