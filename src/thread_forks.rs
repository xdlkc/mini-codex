//! Chat forks keep a coherent rollout prefix and can use a fresh working-tree
//! copy. Source history, goals, schedules and files are never moved by a fork.
use super::*;
use std::collections::{BTreeMap, BTreeSet};

type TitleEntry = (String, Option<String>);
fn numbered_title(title: &str) -> Option<(&str, u64)> {
    let (base, number) = title.strip_suffix(')')?.rsplit_once(" (")?;
    let number = number.parse::<u64>().ok().filter(|number| *number >= 2)?;
    Some((base, number))
}
fn title_with_number(base: &str, number: u64) -> String {
    let suffix = format!(" ({number})");
    let budget = 60usize.saturating_sub(suffix.len());
    let mut text = base.to_string();
    if text.encode_utf16().count() > budget {
        let mut units = 0;
        text = base
            .chars()
            .take_while(|c| {
                units += c.len_utf16();
                units < budget
            })
            .collect::<String>()
            .trim_end()
            .to_string();
        text.push('…');
    }
    format!("{text}{suffix}")
}
fn family_number(title: &str, base: &str) -> Option<u64> {
    if title.trim() == base {
        return Some(1);
    }
    let (_, number) = numbered_title(title.trim())?;
    (title_with_number(base, number) == title.trim()).then_some(number)
}
fn descends_from(id: &str, root: &str, entries: &BTreeMap<String, TitleEntry>) -> bool {
    let mut current = Some(id);
    let mut seen = BTreeSet::new();
    while let Some(id) = current {
        if !seen.insert(id) {
            break;
        }
        if id == root {
            return true;
        }
        current = entries.get(id).and_then(|(_, parent)| parent.as_deref());
    }
    false
}
fn fork_title(source: &str, entries: &BTreeMap<String, TitleEntry>) -> Option<String> {
    let (title, parent) = entries.get(source)?;
    let title = title.trim();
    if title.is_empty() {
        return None;
    }
    let mut base = title;
    let mut root = source;
    if let Some((prefix, number)) = numbered_title(title) {
        if parent.as_ref().is_some_and(|id| !entries.contains_key(id)) {
            base = prefix;
        } else if parent.is_some() {
            let mut ancestor = parent.as_deref();
            let mut seen = BTreeSet::new();
            'ancestors: while let Some(id) = ancestor {
                if !seen.insert(id) {
                    break;
                }
                let Some((name, parent)) = entries.get(id) else {
                    break;
                };
                let name = name.trim();
                for candidate in [Some(name), numbered_title(name).map(|(base, _)| base)]
                    .into_iter()
                    .flatten()
                {
                    if !candidate.is_empty() && title_with_number(candidate, number) == title {
                        base = candidate;
                        root = id;
                        let mut above = parent.as_deref();
                        while let Some(id) = above {
                            if !seen.insert(id) {
                                break;
                            }
                            let Some((name, next)) = entries.get(id) else {
                                break;
                            };
                            if family_number(name, candidate).is_none() {
                                break;
                            }
                            root = id;
                            above = next.as_deref();
                        }
                        break 'ancestors;
                    }
                }
                ancestor = parent.as_deref();
            }
        }
    }
    let maximum = entries
        .iter()
        .filter(|(id, _)| descends_from(id, root, entries))
        .filter_map(|(_, entry)| family_number(&entry.0, base))
        .max()
        .unwrap_or(1);
    Some(title_with_number(base, maximum.saturating_add(1)))
}

impl Server {
    pub(super) async fn fork_thread(self: &Arc<Self>, p: Value) -> RpcResult {
        let _gate = self.workspace_gate.lock().await;
        self.fork_thread_locked(p).await
    }

    // Only a trusted coordinator holding workspace_gate may call this helper.
    pub(super) async fn fork_thread_locked(self: &Arc<Self>, p: Value) -> RpcResult {
        let bad = |message: &str| (-32602, message.to_string());
        let requested = p["threadId"].as_str().ok_or_else(|| bad("threadId"))?;
        let (source, mut items, meta) =
            Session::open(Some(requested)).map_err(|e| (-32000, e.to_string()))?;
        let source_id = source.id.clone();
        if let Some(index) = p.get("beforeUserMessage").filter(|value| !value.is_null()) {
            let index = index
                .as_u64()
                .ok_or_else(|| bad("beforeUserMessage must be an integer"))?;
            let end =
                visible_user_message_index(&items, index).ok_or_else(|| bad("message index"))?;
            items.truncate(end);
        }
        crate::guardian::inherit_approvals(&mut items);
        let destination = p
            .get("destination")
            .filter(|value| !value.is_null())
            .map(|value| {
                value
                    .as_str()
                    .ok_or_else(|| bad("destination must be a string"))
            })
            .transpose()?
            .unwrap_or("current");
        if !["current", "newWorktree"].contains(&destination) {
            return Err(bad("destination must be current or newWorktree"));
        }
        if p.get("titleMode")
            .filter(|value| !value.is_null())
            .is_some_and(|value| value != "fork")
        {
            return Err(bad("titleMode must be fork"));
        }
        let old = desktop::handle(
            "desktop/threadMetadata/read",
            &json!({"threadId":source_id}),
        )
        .unwrap()
        .map_err(|e| (-32000, e))?;
        let mode = old["metadata"]["mode"].as_str().unwrap_or("codex");
        if destination == "newWorktree" && mode != "codex" {
            return Err(bad("A worktree fork requires a Codex chat"));
        }
        let search = saved_search_preference(&old["metadata"]).map_err(|e| (-32000, e))?;
        let model = p
            .get("model")
            .filter(|value| !value.is_null())
            .map(|value| {
                value
                    .as_str()
                    .filter(|s| !s.trim().is_empty() && s.len() <= 256)
                    .ok_or_else(|| bad("Invalid model"))
            })
            .transpose()?
            .unwrap_or_else(|| meta["model"].as_str().unwrap_or(&self.shared.default_model));
        let effort = p
            .get("reasoningEffort")
            .filter(|value| !value.is_null())
            .or_else(|| old["metadata"].get("reasoningEffort"))
            .map(|value| {
                value
                    .as_str()
                    .filter(|value| *value == "auto" || models::EFFORTS.contains(value))
                    .ok_or_else(|| bad("Invalid reasoning effort"))
            })
            .transpose()?;
        if let Some(effort) = effort.filter(|effort| *effort != "auto") {
            if let Some(info) = self
                .models
                .metadata(model)
                .await
                .filter(|info| info["reasoningMetadataKnown"] == true)
            {
                if !info["supportedReasoningEfforts"]
                    .as_array()
                    .is_some_and(|options| {
                        options
                            .iter()
                            .any(|option| option["reasoningEffort"] == effort)
                    })
                {
                    return Err(bad(
                        "Reasoning effort is not supported by the selected model",
                    ));
                }
            }
        }
        let source_cwd = meta["cwd"].as_str().unwrap_or(".");
        let explicit = p
            .get("cwd")
            .filter(|value| !value.is_null())
            .map(|value| {
                value
                    .as_str()
                    .ok_or_else(|| bad("cwd must be a directory path"))
            })
            .transpose()?;
        if destination == "newWorktree" && explicit.is_some() {
            return Err(bad(
                "A new worktree starts from this chat's current directory",
            ));
        }
        let mut cwd = if let Some(path) = explicit {
            let path = PathBuf::from(path);
            if !path.is_absolute() || !path.is_dir() {
                return Err(bad("cwd must be an existing absolute directory"));
            }
            path.canonicalize()
                .map_err(|e| (-32000, e.to_string()))?
                .to_str()
                .ok_or_else(|| bad("cwd is not UTF-8"))?
                .to_string()
        } else {
            source_cwd.to_string()
        };
        let name = if p["titleMode"] == "fork" {
            let all_meta = desktop::handle("desktop/threadMetadata/list", &json!({}))
                .unwrap()
                .map_err(|e| (-32000, e))?;
            let parents: BTreeMap<String, Option<String>> = all_meta["items"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|entry| {
                    Some((
                        entry["threadId"].as_str()?.to_string(),
                        entry["forkedFromId"].as_str().map(String::from),
                    ))
                })
                .collect();
            let mut entries: BTreeMap<String, TitleEntry> = session::list_all()
                .into_iter()
                .map(|entry| {
                    let parent = parents.get(&entry.id).cloned().flatten();
                    (entry.id, (entry.name.unwrap_or(entry.preview), parent))
                })
                .collect();
            entries
                .entry(source_id.clone())
                .or_insert_with(|| (meta["name"].as_str().unwrap_or("").to_string(), None));
            fork_title(&source_id, &entries)
        } else {
            None
        };
        let operation = self.new_id("fork");
        self.notify("thread/fork/progress",json!({"threadId":source_id,"operationId":operation,"stage":if destination=="newWorktree"{"creatingWorktree"}else{"copyingHistory"}}));
        let prepared = if destination == "newWorktree" {
            let workspace =
                workspaces::create_fork_worktree(source_cwd, name.as_deref().unwrap_or("New chat"))
                    .await
                    .map_err(|error| (-32000, error))?;
            cwd = workspace.info["cwd"]
                .as_str()
                .ok_or_else(|| bad("Created worktree has no directory"))?
                .to_string();
            Some(workspace)
        } else {
            None
        };
        let mut created_id = None;
        let mut created_session = None;
        let result:RpcResult=async{
            self.notify("thread/fork/progress",json!({"threadId":source_id,"operationId":operation,"stage":"copyingHistory"}));
            let session=Session::create(&cwd,model).map_err(|e|(-32000,e.to_string()))?;let id=session.id.clone();created_id=Some(id.clone());created_session=Some(session.clone());
            session.append_batch_checked(&items).map_err(|e|(-32000,e.to_string()))?;
            for (position,snapshot) in meta["_mcodex_ui_turns"].as_object().into_iter().flatten(){
                if let Ok(position)=position.parse::<usize>(){if position<items.len(){session.append_ui_snapshot(position,snapshot).map_err(|e|(-32000,e.to_string()))?;}}
            }
            if let Some(name)=&name{session.try_set_name(name).map_err(|e|(-32000,e.to_string()))?;}
            let mut fields=json!({"threadId":id,"mode":mode,"projectId":old["metadata"]["projectId"],"webSearchMode":search.as_str(),"forkedFromId":source_id});
            if let Some(effort)=effort{fields["reasoningEffort"]=json!(effort);}
            desktop::handle("desktop/threadMetadata/set",&fields).unwrap().map_err(|e|(-32000,e))?;
            let (sandbox,auto,reviewer)=self.thread_permissions(&p,&old["metadata"])?;self.save_thread_permissions(&id,sandbox,auto,reviewer)?;let mut agent=self.make_agent(session,items.clone(),model,&cwd,sandbox,auto).await.map_err(|e|(-32000,e.to_string()))?;
            if let Some(effort)=effort{agent.reasoning_effort=effort.to_string();}
            let worktree=if let Some(workspace)=prepared.as_ref(){Some(workspace.commit(&id).await.map_err(|e|(-32000,e))?)}else{None};
            self.register(&id,agent,&cwd,now_ms());
            let thread=self.thread_json(&id,name.as_deref(),"",&cwd,model,now_ms(),now_ms(),false,history_to_turns_with_ui(&items,&cwd,&meta));
            self.notify("thread/started",json!({"thread":thread}));
            self.notify("thread/fork/progress",json!({"threadId":source_id,"operationId":operation,"stage":"ready","targetThreadId":id}));
            Ok(json!({"thread":thread,"cwd":cwd,"worktree":worktree}))
        }.await;
        if let Err(mut error) = result {
            let mut removed = true;
            if let Some(id) = created_id.as_deref() {
                if let Some(session) = created_session.as_ref() {
                    removed = session.delete();
                }
                if removed {
                    if let Err(cleanup) = desktop::discard_failed_thread(id) {
                        error.1.push_str(&format!("; metadata cleanup: {cleanup}"));
                    }
                } else {
                    error
                        .1
                        .push_str(&format!("; the partial chat {id} was retained"));
                }
            }
            if let Some(workspace) = prepared {
                if removed {
                    if let Err(cleanup) = workspace.rollback(created_id).await {
                        error.1.push_str(&format!("; {cleanup}"));
                    }
                } else {
                    error
                        .1
                        .push_str(&format!("; its worktree was retained at {cwd}"));
                }
            }
            self.notify("thread/fork/progress",json!({"threadId":source_id,"operationId":operation,"stage":"error","message":error.1}));
            return Err(error);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn entries(rows: &[(&str, &str, Option<&str>)]) -> BTreeMap<String, TitleEntry> {
        rows.iter()
            .map(|(id, name, parent)| ((*id).into(), ((*name).into(), parent.map(String::from))))
            .collect()
    }
    #[test]
    fn fork_titles_follow_the_reference_family_counter() {
        let mut rows = entries(&[("root", "Design", None), ("unrelated", "Design (99)", None)]);
        assert_eq!(fork_title("root", &rows).as_deref(), Some("Design (2)"));
        rows.insert("child".into(), ("Design (2)".into(), Some("root".into())));
        rows.insert(
            "grandchild".into(),
            ("Design (3)".into(), Some("child".into())),
        );
        assert_eq!(fork_title("root", &rows).as_deref(), Some("Design (4)"));
        assert_eq!(fork_title("child", &rows).as_deref(), Some("Design (4)"));
        rows.get_mut("child").unwrap().0 = "Renamed".into();
        assert_eq!(fork_title("child", &rows).as_deref(), Some("Renamed (2)"));
    }
    #[test]
    fn fork_titles_handle_cycles_missing_parents_and_unicode() {
        let rows = entries(&[
            ("a", "Review (2)", Some("missing")),
            ("b", "Circle", Some("c")),
            ("c", "Circle (2)", Some("b")),
            ("empty", "  ", None),
        ]);
        assert_eq!(fork_title("a", &rows).as_deref(), Some("Review (3)"));
        assert_eq!(fork_title("empty", &rows), None);
        assert_eq!(fork_title("b", &rows).as_deref(), Some("Circle (3)"));
        let title = title_with_number(&"任务🙂".repeat(30), 2);
        assert!(title.encode_utf16().count() <= 60);
        assert!(title.ends_with("… (2)"));
    }
}
