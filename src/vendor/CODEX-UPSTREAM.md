# Codex policy sources

Source: https://github.com/openai/codex/tree/3f31c0a040ca58b86cea70a8d40ad79d481e45d2
Commit: `3f31c0a040ca58b86cea70a8d40ad79d481e45d2`
License: Apache-2.0. `LICENSE-CODEX.txt` includes the upstream notice.

- `codex-execpolicy`: upstream execution policy parser, matching, and rule persistence. Sources and tests are unchanged. The manifest uses explicit dependencies for this project.
- `codex-absolute-path`: upstream path support. Sources are unchanged. The manifest uses explicit dependencies.
- `codex-shell-parser`: upstream Bash/PowerShell parsing, dangerous-command classification, and the shell type definitions/detection used by those parsers. The library entry point and manifest expose this subset. The test-only external PowerShell subprocess oracle is omitted; grammar fixtures are retained. `prefix_suggestions.rs` copies `BANNED_PREFIX_SUGGESTIONS` from `core/src/exec_policy.rs`.

- `codex-guardian`: unchanged default policy template, tenant policy, and output contract from `prompts/templates/guardian` and `ext/guardian-reviewer/src/assessment.rs`; transcript provenance text from `guardian-context/src/transcript_record.rs`. The integration in `guardian.rs` also follows the manual-approval developer context from `core/src/context/guardian_approved_action.rs` and the catalog selection in `ext/guardian-reviewer/src/model.rs`.

The public source revision is not claimed to equal the installed desktop build. Compatibility is checked against the locally installed Codex CLI.
