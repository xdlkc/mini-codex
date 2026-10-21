//! Approval decisions use the pinned Codex Starlark engine and shell parsers.
//! Policies belong to this app's profile; the official Codex profile is read-only.
use anyhow::{Context, Result};
use codex_execpolicy::{Decision, MatchOptions, Policy, PolicyParser, RuleMatch};
use codex_shell_parser::{bash, is_dangerous_command as dangerous, powershell};
use std::fs::{self, OpenOptions};
use std::path::Path;

pub struct Evaluation {
    pub bypass_sandbox: bool,
    pub needs_approval: bool,
    pub rejection: Option<String>,
    pub reason: Option<String>,
    pub amendment: Option<Vec<String>>,
    pub warning: Option<String>,
}

fn load(home: Option<&Path>) -> Result<(Policy, Option<String>)> {
    let Some(home) = home else {
        return Ok((Policy::empty(), None));
    };
    let directory = home.join("rules");
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((Policy::empty(), None))
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("Read command policies in {}", directory.display()))
        }
    };
    let mut files = Vec::new();
    for entry in entries {
        let entry = entry?;
        if entry
            .path()
            .extension()
            .is_some_and(|extension| extension == "rules")
            && entry.file_type()?.is_file()
        {
            files.push(entry.path());
        }
    }
    files.sort();
    let mut parser = PolicyParser::new();
    for path in files {
        let contents = fs::read_to_string(&path)
            .with_context(|| format!("Read command policy {}", path.display()))?;
        if let Err(error) = parser.parse(&path.to_string_lossy(), &contents) {
            // Codex discards the user rules after a parse failure, reports the
            // warning, and continues to enforce the original sandbox.
            return Ok((
                Policy::empty(),
                Some(format!("Command rules were not loaded: {error}")),
            ));
        }
    }
    Ok((parser.build(), None))
}

#[derive(Clone, Copy)]
enum Origin {
    Generic,
    PowerShell,
    PowerShellLiteral,
}

fn commands(argv: &[String]) -> (Vec<Vec<String>>, Origin) {
    if let Some(commands) =
        bash::parse_shell_lc_plain_commands(argv).filter(|commands| !commands.is_empty())
    {
        return (commands, Origin::Generic);
    }
    if cfg!(windows) {
        if let Some((_, script)) = powershell::extract_powershell_command(argv) {
            if let Some(commands) = powershell::parse_powershell_script_into_plain_commands(script)
                .filter(|commands| !commands.is_empty())
            {
                let origin = if dangerous::powershell_commands_are_independent(script, &commands) {
                    Origin::PowerShellLiteral
                } else {
                    Origin::PowerShell
                };
                return (commands, origin);
            }
        }
    }
    (vec![argv.to_vec()], Origin::Generic)
}

fn danger(command: &[String], origin: Origin) -> Option<dangerous::DangerousCommandMatch> {
    match origin {
        Origin::Generic => dangerous::dangerous_command_match(command),
        Origin::PowerShell => dangerous::dangerous_powershell_words_match(
            command,
            dangerous::DangerousCommandPlatform::host(),
        ),
        Origin::PowerShellLiteral => dangerous::dangerous_powershell_literal_words_match(command),
    }
}

fn policy_match(rule: &RuleMatch) -> bool {
    matches!(rule, RuleMatch::PrefixRuleMatch { .. })
}

pub fn evaluate(
    home: Option<&Path>,
    argv: &[String],
    requested_prefix: Option<&[String]>,
    escalated: bool,
    never: bool,
) -> Result<Evaluation> {
    let (policy, warning) = load(home)?;
    let (commands, origin) = commands(argv);
    let fallback = |command: &[String]| {
        if danger(command, origin).is_some() {
            if never {
                Decision::Forbidden
            } else {
                Decision::Prompt
            }
        } else if escalated && !never {
            Decision::Prompt
        } else {
            Decision::Allow
        }
    };
    let options = MatchOptions {
        resolve_host_executables: true,
    };
    let evaluation = policy.check_multiple_with_options(commands.iter(), &fallback, &options);
    let specific = |decision| {
        evaluation
            .matched_rules
            .iter()
            .filter_map(|rule| match rule {
                RuleMatch::PrefixRuleMatch {
                    matched_prefix,
                    decision: actual,
                    justification,
                    ..
                } if *actual == decision => Some((matched_prefix, justification.as_deref())),
                _ => None,
            })
            .max_by_key(|(prefix, _)| prefix.len())
    };
    let rendered = crate::tools::command_text(argv);
    let rejection = if evaluation.decision == Decision::Forbidden {
        Some(match specific(Decision::Forbidden) {
            Some((_, Some(reason))) => format!("`{rendered}` rejected: {reason}"),
            Some((prefix, None)) => format!(
                "`{rendered}` rejected: policy forbids commands starting with `{}`",
                crate::tools::command_text(prefix)
            ),
            None => {
                let forced_rm = commands.iter().any(|command| {
                    danger(command, origin) == Some(dangerous::DangerousCommandMatch::ForcedRm)
                });
                format!(
                    "`{rendered}` rejected: {}",
                    if forced_rm {
                        "rm -f style commands are not permitted. Use a safer approach"
                    } else {
                        "blocked by policy"
                    }
                )
            }
        })
    } else if evaluation.decision == Decision::Prompt && never {
        Some("approval required by policy, but AskForApproval is set to Never".into())
    } else {
        None
    };
    let reason = specific(Decision::Prompt).map(|(_, reason)| match reason {
        Some(reason) => format!("`{rendered}` requires approval: {reason}"),
        None => format!("`{rendered}` requires approval by policy"),
    });
    let requested = requested_prefix
        .filter(|prefix| !prefix.is_empty())
        .filter(|prefix| {
            !codex_shell_parser::prefix_suggestions::BANNED_PREFIX_SUGGESTIONS
                .iter()
                .any(|banned| prefix.iter().map(String::as_str).eq(banned.iter().copied()))
        })
        .filter(|_| !evaluation.matched_rules.iter().any(policy_match))
        .and_then(|prefix| {
            let mut proposed = policy.clone();
            proposed.add_prefix_rule(prefix, Decision::Allow).ok()?;
            commands
                .iter()
                .all(|command| {
                    proposed
                        .check_with_options(command, &fallback, &options)
                        .decision
                        == Decision::Allow
                })
                .then(|| prefix.to_vec())
        });
    let amendment = if home.is_none() || rejection.is_some() {
        None
    } else {
        requested.or_else(|| {
            let may_amend = if evaluation.decision == Decision::Prompt {
                !evaluation
                    .matched_rules
                    .iter()
                    .any(|rule| policy_match(rule) && rule.decision() == Decision::Prompt)
            } else {
                !evaluation.matched_rules.iter().any(policy_match)
            };
            may_amend
                .then(|| {
                    evaluation.matched_rules.iter().find_map(|rule| match rule {
                        RuleMatch::HeuristicsRuleMatch { command, decision }
                            if *decision == evaluation.decision =>
                        {
                            Some(command.clone())
                        }
                        _ => None,
                    })
                })
                .flatten()
        })
    };
    let bypass_sandbox = evaluation.decision == Decision::Allow
        && commands.iter().all(|command| {
            policy
                .matches_for_command_with_options(command, None, &options)
                .iter()
                .any(|rule| policy_match(rule) && rule.decision() == Decision::Allow)
        });
    Ok(Evaluation {
        bypass_sandbox,
        needs_approval: evaluation.decision == Decision::Prompt,
        rejection,
        reason,
        amendment,
        warning,
    })
}

pub fn persist(home: &Path, prefix: &[String]) -> Result<()> {
    anyhow::ensure!(
        !prefix.is_empty(),
        "Command rule requires a non-empty prefix"
    );
    fs::create_dir_all(home)?;
    let home = home.canonicalize()?;
    let directory = home.join("rules");
    fs::create_dir_all(&directory)?;
    anyhow::ensure!(
        !fs::symlink_metadata(&directory)?.file_type().is_symlink(),
        "Command rule directory must not be a symlink"
    );
    let path = directory.join("default.rules");
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(&path) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => anyhow::ensure!(
            fs::symlink_metadata(&path)?.file_type().is_file(),
            "Command rule target must be a regular file"
        ),
        Err(error) => return Err(error.into()),
    }
    codex_execpolicy::blocking_append_allow_prefix_rule(&path, prefix)
        .context("Save approved command rule")
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture(std::path::PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "mcodex-command-policy-{:032x}",
                rand::random::<u128>()
            ));
            fs::create_dir_all(path.join("rules")).unwrap();
            Self(path)
        }
        fn rules(&self, name: &str, text: &str) {
            fs::write(self.0.join("rules").join(name), text).unwrap();
        }
        fn check(&self, args: &[&str], escalated: bool, never: bool) -> Evaluation {
            evaluate(Some(&self.0), &words(args), None, escalated, never).unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn words(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn starlark_rules_support_alternatives_functions_and_forbidden_precedence() {
        let f = Fixture::new();
        f.rules("default.rules",r#"
def allow_read_commands(program):
    prefix_rule(pattern=[program, ["status", "diff"]], decision="allow", match=[[program, "status"], [program, "diff"]], not_match=[[program, "push"]])
allow_read_commands("git")
prefix_rule(pattern=["git", "diff", "--no-index"], decision="forbidden", justification="Keep this command inside the repository")
"#);
        assert!(
            f.check(&["git", "status", "--short"], true, false)
                .bypass_sandbox
        );
        assert!(
            f.check(&["/usr/bin/git", "status"], true, false)
                .bypass_sandbox
        );
        let blocked = f.check(&["git", "diff", "--no-index", "a", "b"], true, false);
        assert!(blocked
            .rejection
            .unwrap()
            .contains("Keep this command inside the repository"));
        assert!(f.check(&["git", "push"], true, false).needs_approval);
    }
    #[test]
    fn every_shell_segment_needs_an_explicit_allow_before_bypassing_the_sandbox() {
        let f = Fixture::new();
        f.rules(
            "default.rules",
            "prefix_rule(pattern=[\"git\",\"status\"], decision=\"allow\")\n",
        );
        let partial = f.check(
            &["/bin/sh", "-c", "git status && touch outside"],
            false,
            false,
        );
        assert!(!partial.bypass_sandbox);
        assert!(!partial.needs_approval);
        let escalated = f.check(
            &["/bin/sh", "-c", "git status && touch outside"],
            true,
            false,
        );
        assert!(escalated.needs_approval);
        assert_eq!(escalated.amendment, Some(words(&["touch", "outside"])));
        f.rules(
            "more.rules",
            "prefix_rule(pattern=[\"touch\",\"outside\"], decision=\"allow\")\n",
        );
        assert!(
            f.check(
                &["/bin/sh", "-c", "git status && touch outside"],
                false,
                false
            )
            .bypass_sandbox
        );
        assert!(
            !f.check(&["/bin/sh", "-c", "git status > outside"], false, false)
                .bypass_sandbox
        );
    }
    #[test]
    fn reusable_prefixes_must_cover_the_actual_invocation() {
        let f = Fixture::new();
        let args = words(&["/bin/sh", "-c", "touch file && git status"]);
        let accepted = evaluate(Some(&f.0), &args, Some(&words(&["touch"])), true, false).unwrap();
        assert_eq!(accepted.amendment, Some(words(&["touch", "file"])));
        let all = evaluate(
            Some(&f.0),
            &words(&["git", "push", "origin", "main"]),
            Some(&words(&["git", "push"])),
            true,
            false,
        )
        .unwrap();
        assert_eq!(all.amendment, Some(words(&["git", "push"])));
        let broad = evaluate(
            Some(&f.0),
            &words(&["git", "push", "origin", "main"]),
            Some(&words(&["git"])),
            true,
            false,
        )
        .unwrap();
        assert_eq!(
            broad.amendment,
            Some(words(&["git", "push", "origin", "main"]))
        );
        f.rules("prompt.rules","prefix_rule(pattern=[\"git\",\"push\"],decision=\"prompt\",justification=\"Review the remote\")\n");
        let forced = f.check(&["git", "push", "origin", "main"], true, false);
        assert!(forced.amendment.is_none());
        assert!(forced.reason.unwrap().contains("Review the remote"));
    }
    #[test]
    fn dangerous_command_classification_and_never_match_the_reference() {
        let f = Fixture::new();
        let args = ["/bin/sh", "-c", "if true; then env X=1 rm -rf owned; fi"];
        assert!(f.check(&args, false, false).needs_approval);
        assert!(f
            .check(&args, false, true)
            .rejection
            .unwrap()
            .contains("rm -f style commands"));
        f.rules(
            "default.rules",
            "prefix_rule(pattern=[\"rm\",\"-rf\",\"owned\"],decision=\"allow\")\n",
        );
        assert!(f.check(&["rm", "-rf", "owned"], false, true).bypass_sandbox);
        assert!(
            f.check(&["/bin/sh", "-c", "trap 'rm -f owned' EXIT"], false, false)
                .needs_approval
        );
    }
    #[test]
    fn approved_rules_survive_reload_are_argv_scoped_and_do_not_cross_profiles() {
        let f = Fixture::new();
        let other = Fixture::new();
        let prefix = words(&["echo", "quoted ' value\nsecond"]);
        persist(&f.0, &prefix).unwrap();
        assert!(
            evaluate(Some(&f.0), &prefix, None, true, false)
                .unwrap()
                .bypass_sandbox
        );
        assert!(
            !evaluate(Some(&other.0), &prefix, None, true, false)
                .unwrap()
                .bypass_sandbox
        );
        assert!(!f.check(&["echo", "different"], true, false).bypass_sandbox);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(f.0.join("rules/default.rules"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }
    #[test]
    fn malformed_policy_warns_discards_partial_rules_and_keeps_the_sandbox() {
        let f = Fixture::new();
        f.rules(
            "a.rules",
            "prefix_rule(pattern=[\"touch\"],decision=\"allow\")\n",
        );
        f.rules("z.rules", "prefix_rule(pattern=[");
        let result = f.check(&["touch", "outside"], true, false);
        assert!(result.warning.unwrap().contains("z.rules"));
        assert!(!result.bypass_sandbox);
        assert!(result.needs_approval);
        fs::remove_file(f.0.join("rules/z.rules")).unwrap();
        assert!(f.check(&["touch", "outside"], true, false).bypass_sandbox);
        fs::remove_dir_all(f.0.join("rules")).unwrap();
        fs::write(f.0.join("rules"), "not a directory").unwrap();
        assert!(evaluate(Some(&f.0), &words(&["touch", "outside"]), None, true, false).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn policy_loading_and_persistence_do_not_follow_rule_file_symlinks() {
        let f = Fixture::new();
        let other = Fixture::new();
        let outside = other.0.join("keep");
        fs::write(&outside, "unchanged").unwrap();
        std::os::unix::fs::symlink(&outside, f.0.join("rules/default.rules")).unwrap();
        assert!(!f.check(&["touch", "outside"], true, false).bypass_sandbox);
        assert!(persist(&f.0, &words(&["touch"])).is_err());
        assert_eq!(fs::read_to_string(outside).unwrap(), "unchanged");
    }
}
