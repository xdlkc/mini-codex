//! Terminal consumers share the GUI's authoritative message snapshots without
//! printing text twice when a final output repeats streamed deltas.
use serde_json::Value;
use std::collections::HashMap;

#[derive(Default)]
pub(crate) struct StreamDisplay {
    active: Option<String>,
    shown: HashMap<String, String>,
}
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Update {
    Append(String),
    Replace(String),
}
impl StreamDisplay {
    pub(crate) fn delta(&mut self, text: &str) {
        if let Some(key) = &self.active {
            self.shown.entry(key.clone()).or_default().push_str(text);
        }
    }
    pub(crate) fn message(&mut self, value: &Value) -> Option<Update> {
        let key = value["_mcodex_item_key"]
            .as_str()
            .or(value["id"].as_str())
            .unwrap_or("anonymous")
            .to_string();
        if value["_mcodex_activate"] == true {
            self.active = Some(key);
            return None;
        }
        let projected = crate::web_search::project_message(value).ok()?;
        let text = projected["text"].as_str()?.to_string();
        let shown = self
            .shown
            .insert(key.clone(), text.clone())
            .unwrap_or_default();
        if self.active.as_deref() == Some(key.as_str()) {
            self.active = None;
        }
        if text == shown {
            return None;
        }
        Some(match text.strip_prefix(&shown) {
            Some(suffix) => Update::Append(suffix.to_string()),
            None => Update::Replace(text),
        })
    }
    pub(crate) fn clear(&mut self) {
        self.active = None;
        self.shown.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn active(key: &str) -> Value {
        json!({"type":"message","role":"assistant","content":[],"_mcodex_item_key":key,"_mcodex_activate":true})
    }
    fn message(key: &str, text: &str) -> Value {
        json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":text}],"_mcodex_item_key":key})
    }
    #[test]
    fn final_only_unicode_is_shown_once_and_richer_metadata_does_not_repeat_text() {
        let mut display = StreamDisplay::default();
        let mut final_item = message("r:1", "中文🙂");
        assert_eq!(display.message(&active("r:1")), None);
        assert_eq!(
            display.message(&final_item),
            Some(Update::Append("中文🙂".into()))
        );
        final_item["content"][0]["annotations"] = json!([]);
        assert_eq!(display.message(&final_item), None);
    }
    #[test]
    fn interleaved_messages_receive_only_their_missing_suffix() {
        let mut display = StreamDisplay::default();
        display.message(&active("r:a"));
        display.delta("前🙂");
        display.message(&active("r:b"));
        display.delta("B");
        assert_eq!(
            display.message(&message("r:a", "前🙂后")),
            Some(Update::Append("后".into()))
        );
        assert_eq!(
            display.message(&message("r:b", "Body")),
            Some(Update::Append("ody".into()))
        );
    }
    #[test]
    fn equal_text_in_different_messages_is_not_discarded() {
        let mut display = StreamDisplay::default();
        for key in ["r:a", "r:b"] {
            assert_eq!(
                display.message(&message(key, "same")),
                Some(Update::Append("same".into()))
            );
        }
    }
    #[test]
    fn changed_final_is_an_explicit_replacement_and_turn_state_can_be_cleared() {
        let mut display = StreamDisplay::default();
        display.message(&active("r:a"));
        display.delta("draft");
        assert_eq!(
            display.message(&message("r:a", "corrected")),
            Some(Update::Replace("corrected".into()))
        );
        display.clear();
        assert_eq!(
            display.message(&message("r:a", "corrected")),
            Some(Update::Append("corrected".into()))
        );
    }
}
