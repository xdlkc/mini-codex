//! Validated presentation of Responses hosted search. Raw provider items stay in
//! model history; these projections contain only fields safe to display as links.
use serde_json::{json, Value};
type Result<T> = std::result::Result<T, String>;
const MAX_BYTES: usize = 8 * 1024 * 1024;
const MAX_LINKS: usize = 1024;
const MAX_PARTS: usize = 1024;
const MAX_QUERIES: usize = 128;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum SearchMode {
    #[default]
    Disabled,
    Auto,
    Cached,
    Live,
    Required,
}
impl SearchMode {
    pub(crate) fn parse(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "disabled" => Ok(Self::Disabled),
            "auto" => Ok(Self::Auto),
            "cached" => Ok(Self::Cached),
            "live" => Ok(Self::Live),
            "required" => Ok(Self::Required),
            _ => Err("webSearchMode must be disabled, auto, cached, live or required".into()),
        }
    }
    pub(crate) fn from_value(value: &Value) -> Result<Self> {
        match value {
            Value::Bool(true) => Ok(Self::Auto),
            Value::Bool(false) => Ok(Self::Disabled),
            Value::String(value) => Self::parse(value),
            _ => Err("Web search preference must be a mode string or boolean".into()),
        }
    }
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Auto => "auto",
            Self::Cached => "cached",
            Self::Live => "live",
            Self::Required => "required",
        }
    }
    pub(crate) fn enabled(self) -> bool {
        self != Self::Disabled
    }
    pub(crate) fn tool(self) -> Option<Value> {
        match self {
            Self::Disabled => None,
            Self::Auto => Some(json!({"type":"web_search"})),
            Self::Cached => Some(json!({"type":"web_search","external_web_access":false})),
            Self::Live | Self::Required => {
                Some(json!({"type":"web_search","external_web_access":true}))
            }
        }
    }
}

fn text(value: &Value, field: &str, max: usize) -> Result<String> {
    let text = value
        .as_str()
        .ok_or_else(|| format!("Search {field} must be a string"))?;
    if text.len() > max
        || text
            .chars()
            .any(|c| c == '\0' || c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
    {
        return Err(format!(
            "Search {field} exceeds its length or character limit"
        ));
    }
    Ok(text.into())
}
fn bounded(value: &Value) -> Result<()> {
    if serde_json::to_vec(value)
        .map_err(|_| "Invalid search metadata")?
        .len()
        > MAX_BYTES
    {
        return Err("Search metadata exceeds 8 MiB".into());
    }
    Ok(())
}
fn id(value: &Value) -> Result<String> {
    let id = text(value, "item id", 256)?;
    if id.is_empty() || id.chars().any(char::is_whitespace) {
        return Err("Invalid search item id".into());
    }
    Ok(id)
}
/// This validates a navigation target only. No URL is fetched by this module.
pub(crate) fn safe_url(value: &str) -> Option<String> {
    if value.is_empty()
        || value.len() > 8192
        || value
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || c == '\\')
    {
        return None;
    }
    let parsed = reqwest::Url::parse(value).ok()?;
    if !matches!(parsed.scheme(), "http" | "https")
        || !parsed.has_host()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return None;
    }
    Some(value.into())
}
fn url(value: &Value) -> Result<String> {
    value
        .as_str()
        .and_then(safe_url)
        .ok_or_else(|| "Search URL must be a safe absolute HTTP(S) URL without credentials".into())
}
fn source(value: &Value) -> Result<Value> {
    if !value.is_object() || value.get("type").is_some_and(|v| v != "url") {
        return Err("Invalid web search source".into());
    }
    let mut result = json!({"url":url(&value["url"])?});
    if let Some(title) = value.get("title") {
        result["title"] = json!(text(title, "source title", 4096)?);
    }
    Ok(result)
}
/// A real provider call/progress item. No query, source or success is inferred
/// from the assistant's prose. Unknown provider fields remain only in raw history.
pub(crate) fn project_call(item: &Value) -> Result<Value> {
    bounded(item)?;
    if item["type"] != "web_search_call" {
        return Err("Expected a web_search_call output item".into());
    }
    let provider_id = id(&item["id"])?;
    let status = item["status"]
        .as_str()
        .filter(|s| {
            matches!(
                *s,
                "in_progress" | "searching" | "completed" | "failed" | "incomplete" | "interrupted"
            )
        })
        .ok_or("Invalid web search status")?;
    let mut result = json!({"type":"webSearch","providerId":provider_id,"status":status,"queries":[],"sources":[]});
    if let Some(action) = item.get("action").filter(|v| !v.is_null()) {
        let kind = action["type"]
            .as_str()
            .filter(|s| matches!(*s, "search" | "open_page" | "find_in_page"))
            .ok_or("Invalid web search action")?;
        result["actionType"] = json!(kind);
        if let Some(queries) = action.get("queries").filter(|v| !v.is_null()) {
            let queries = queries
                .as_array()
                .filter(|v| v.len() <= MAX_QUERIES)
                .ok_or("Web search queries exceed their item limit or are invalid")?;
            result["queries"] = json!(queries
                .iter()
                .map(|q| text(q, "query", 8192))
                .collect::<Result<Vec<_>>>()?);
        }
        if let Some(query) = action.get("query").filter(|v| !v.is_null()) {
            let query = text(query, "query", 8192)?;
            if result["queries"].as_array().unwrap().is_empty() {
                result["queries"] = json!([query]);
            }
        }
        if let Some(sources) = action.get("sources").filter(|v| !v.is_null()) {
            let sources = sources
                .as_array()
                .filter(|v| v.len() <= MAX_LINKS)
                .ok_or("Web search sources exceed their item limit or are invalid")?;
            result["sources"] = json!(sources.iter().map(source).collect::<Result<Vec<_>>>()?);
        }
        if let Some(value) = action.get("url").filter(|v| !v.is_null()) {
            result["url"] = json!(url(value)?);
        }
        if let Some(pattern) = action.get("pattern").filter(|v| !v.is_null()) {
            result["pattern"] = json!(text(pattern, "find pattern", 8192)?);
        }
    }
    Ok(result)
}
fn citation(annotation: &Value) -> Result<Value> {
    let start = annotation["start_index"]
        .as_u64()
        .ok_or("Citation start_index must be a nonnegative integer")?;
    let end = annotation["end_index"]
        .as_u64()
        .ok_or("Citation end_index must be a nonnegative integer")?;
    if start > end || end > MAX_BYTES as u64 {
        return Err("Citation index range is invalid or exceeds the text bound".into());
    }
    Ok(
        json!({"type":"url_citation","url":url(&annotation["url"])?,"title":text(&annotation["title"],"citation title",4096)?,"startIndex":start,"endIndex":end,"indexUnit":"provider"}),
    )
}
/// Validate but never guess the provider's Unicode index unit. Inline link
/// placement requires a confirmed unit; callers can always show explicit links.
pub(crate) fn project_message(item: &Value) -> Result<Value> {
    bounded(item)?;
    if item["type"] != "message" || item["role"] != "assistant" {
        return Err("Expected an assistant message output item".into());
    }
    let provider_id = item.get("id").map(id).transpose()?;
    let content = item["content"]
        .as_array()
        .filter(|v| v.len() <= MAX_PARTS)
        .ok_or("Assistant message content is invalid or exceeds its item limit")?;
    let mut parts = Vec::new();
    let mut all = Vec::new();
    let mut combined = String::new();
    for (index, part) in content.iter().enumerate() {
        let kind = part["type"]
            .as_str()
            .ok_or("Assistant content requires a type")?;
        let value = match kind {
            "output_text" => text(&part["text"], "assistant text", MAX_BYTES)?,
            "refusal" => text(&part["refusal"], "assistant refusal", MAX_BYTES)?,
            _ => return Err("Unsupported assistant output content type".into()),
        };
        let mut citations = Vec::new();
        if let Some(annotations) = part.get("annotations") {
            let annotations = annotations
                .as_array()
                .filter(|v| v.len() <= MAX_LINKS)
                .ok_or("Assistant annotations are invalid or exceed their item limit")?;
            for annotation in annotations {
                if !annotation.is_object() || !annotation["type"].is_string() {
                    return Err("Invalid assistant annotation".into());
                }
                if annotation["type"] == "url_citation" {
                    if all.len() >= MAX_LINKS {
                        return Err("Assistant citations exceed their item limit".into());
                    }
                    let mut link = citation(annotation)?;
                    link["contentIndex"] = json!(index);
                    citations.push(link.clone());
                    all.push(link);
                }
            }
        }
        combined.push_str(&value);
        if combined.len() > MAX_BYTES {
            return Err("Assistant message text exceeds 8 MiB".into());
        }
        parts.push(json!({"type":kind,"text":value,"citations":citations}));
    }
    Ok(json!({"providerId":provider_id,"parts":parts,"text":combined,"citations":all}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn links_and_metadata_are_validated_without_inventing_sources() {
        for value in [
            "javascript:alert(1)",
            "data:text/html,evil",
            "file:///etc/passwd",
            "//example.test/x",
            "https://user:pass@example.test",
            "https://example.test/\nnext",
            "https:\\example.test/path",
        ] {
            assert!(safe_url(value).is_none(), "{value}");
        }
        assert!(safe_url("https://example.test/a?b=1#part").is_some());
        let call = json!({"type":"web_search_call","id":"ws_1","status":"completed","action":{"type":"search","queries":["中文\nquery"],"sources":[{"type":"url","url":"https://example.test/a"}]}});
        assert_eq!(
            project_call(&call).unwrap()["sources"][0],
            json!({"url":"https://example.test/a"})
        );
        assert!(project_call(&json!({"type":"web_search_call","id":"ws_1","status":"completed","action":{"type":"search","sources":[{"type":"url","url":"javascript:bad"}]}})).is_err());
        assert_eq!(
            project_call(&json!({"type":"web_search_call","id":"ws_1","status":"failed"})).unwrap()
                ["sources"],
            json!([])
        );
        assert!(project_call(
            &json!({"type":"web_search_call","id":"bad id","status":"completed"})
        )
        .is_err());
        assert!(project_call(&json!({"type":"web_search_call","id":"ws_1","status":"completed","action":{"type":"search","sources":vec![json!({"url":"https://example.test"});MAX_LINKS+1]}})).is_err());
    }
    #[test]
    fn multi_part_citations_preserve_unknown_unicode_units_and_raw_values() {
        let item = json!({"type":"message","id":"msg_1","role":"assistant","content":[{"type":"output_text","text":"中文🙂A","annotations":[{"type":"url_citation","url":"https://example.test","title":"资料","start_index":2,"end_index":4}]},{"type":"output_text","text":"第二段","annotations":[]}]});
        let before = item.clone();
        let display = project_message(&item).unwrap();
        assert_eq!(display["text"], "中文🙂A第二段");
        assert_eq!(display["parts"].as_array().unwrap().len(), 2);
        assert_eq!(display["citations"][0]["indexUnit"], "provider");
        assert_eq!(display["citations"][0]["startIndex"], 2);
        assert_eq!(item, before);
        let mut invalid = item.clone();
        invalid["content"][0]["annotations"][0]["end_index"] = json!(-1);
        assert!(project_message(&invalid).is_err());
        invalid["content"][0]["annotations"][0]["end_index"] = json!(1);
        assert!(project_message(&invalid).is_err());
        invalid["content"][0]["annotations"][0]["end_index"] = json!(4);
        invalid["content"][0]["annotations"][0]["url"] = json!("javascript:alert(1)");
        assert!(project_message(&invalid).is_err());
    }
    #[test]
    fn search_modes_have_explicit_cached_live_and_legacy_boolean_contracts() {
        assert_eq!(SearchMode::default(), SearchMode::Disabled);
        assert_eq!(
            SearchMode::from_value(&json!(false)).unwrap(),
            SearchMode::Disabled
        );
        assert_eq!(
            SearchMode::from_value(&json!(true)).unwrap(),
            SearchMode::Auto
        );
        for mode in [
            SearchMode::Disabled,
            SearchMode::Auto,
            SearchMode::Cached,
            SearchMode::Live,
            SearchMode::Required,
        ] {
            assert_eq!(SearchMode::parse(mode.as_str()).unwrap(), mode);
            assert_eq!(mode.enabled(), mode.tool().is_some());
        }
        assert_eq!(
            SearchMode::Cached.tool().unwrap()["external_web_access"],
            false
        );
        assert_eq!(
            SearchMode::Live.tool().unwrap()["external_web_access"],
            true
        );
        assert_eq!(
            SearchMode::Required.tool().unwrap()["external_web_access"],
            true
        );
        assert!(SearchMode::Auto
            .tool()
            .unwrap()
            .get("external_web_access")
            .is_none());
        assert!(SearchMode::from_value(&json!(null)).is_err());
        assert!(SearchMode::parse("unrecognized").is_err());
    }
}
