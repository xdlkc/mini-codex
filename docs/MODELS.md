# Codex App model and configuration parity

Verified on 2026-10-20 against the installed Codex App `26.1013.40934`, using its bundled app-server `0.163.0-alpha.10.2`. The installed CLI on PATH was a different version. The App backend is the reference for this change.

A read-only comparison queried `model/list` from the App backend and Mini. All 9 metadata entries matched: model IDs, display names, hidden status, reasoning levels and descriptions, defaults, and service tiers. Seven models appear in the normal picker, in this order:

1. GPT-6.1 Sol
2. GPT-6 Astra
3. GPT-6 Sol
4. GPT-6 Luna
5. GPT-5.6 Sol
6. GPT-5.6 Terra
7. GPT-5.6 Luna

The picker uses catalog order and hides internal models. It does not sort alphabetically or put the selected model first. Display names use the App's readable form. A configured custom model remains available.

## Reasoning and speed

The composer and configuration page use the selected model's reported levels. Current Sol/Astra/Terra models report `low`, `medium`, `high`, `xhigh`, `max`, and `ultra`. Luna models stop at `max`. Known models have no synthetic Auto entry. Switching models keeps a supported effort or uses the new model's default. Unknown custom providers retain Auto and a clearly labeled manual value.

`ultra` is preserved through settings, scheduled tasks, document suggestions and the Responses request. The UI does not rename it to max or xhigh. This is parameter parity; no claim is made that Mini implements every Codex orchestration feature associated with that level.

Speed choices use `serviceTiers` and `defaultServiceTier` from the catalog. Fast writes `priority`, not a made-up `fast` wire value. Standard clears the request override. A failed save does not change the displayed selection. Writes are serialized, and a late response from another host cannot replace the current selection. Each outgoing message captures its own model, effort and speed before asynchronous thread creation.

## Defaults and storage

Mini reads only `model`, `model_reasoning_effort` and `service_tier` from `CODEX_HOME/config.toml` (default `~/.codex/config.toml`), including the active profile. Mini's own values in `~/.mcodex/config.toml` take precedence. It does not inherit permissions, tools or instructions from that file. The Use Codex model defaults button clears only the three local overrides. Mini never writes the original Codex configuration.

The actual local comparison matched `gpt-6.1-sol`, `xhigh`, and `priority`. The model's catalog default (`low` for GPT-6.1 Sol) is distinct from the user's configured preference (`xhigh`). `MCODEX_NO_CODEX_IMPORT=1` disables Codex import for isolated use.

## Catalog sources

ChatGPT connections fetch the Codex models endpoint with `client_version=0.163.0`. The returned metadata is cached for five minutes. Failed refreshes retain a clearly stale same-account catalog. Credential and provider changes cannot reuse another account's cache.

For standard OpenAI API-key or offline use, Mini uses sanitized model metadata from the local Codex catalog cache, then the bundled snapshot in `src/vendor/codex-models.json`. It does not use the general Models API as a Codex picker: that endpoint also contains image, audio and embedding models. A local or bundled entry is not proof that the account can call it. Only display metadata is read; model instructions and private message templates are not exported.

Custom provider connections still use that provider's Models API. Unknown Responses, image and reasoning capabilities remain unknown. Manual model IDs are supported. Downloads are limited to 4 MiB, redirects are disabled, and credential changes invalidate pending results. Errors do not include provider response bodies or secrets.

## Verification

- Rust model/config tests cover the App snapshot, Ultra versus Luna limits, configured defaults, active profiles and permission isolation.
- Frontend tests cover reported effort options, real speed values, failure rollback, serialized selection writes and same-path remote host races.
- Model smoke: 4 groups, 20 RPCs and 10 loopback requests. Turn smoke: 8 groups, 45 RPCs and 13 loopback model requests. The latter confirms Ultra/priority reach the provider and Standard removes priority.
- Read-only real App metadata comparison: 9 matching entries, 7 visible models, matching model/effort/speed defaults. No paid inference request was sent.
- Later send-failure verification used real GPT-6 Luna / xhigh / priority requests after enabling macOS system proxy support. Both an isolated RPC turn and the packaged App binary with its full frontend received and saved `OK`. The transport reads system proxy settings and keeps TLS certificate verification enabled.
- Browser and native WebKit menus were exercised. Model switches, Luna effort fallback, Standard/Fast persistence, refresh and the Use Codex model defaults action passed. Restoring defaults did not modify the source Codex configuration. Full regression: 207 Rust tests and 206 frontend tests passed. Screenshots: [reasoning menu](screenshots/30-codex-reasoning-parity.png), [configuration](screenshots/31-codex-settings-parity.png).

Read-only installed App comparison: `python3 scripts/check-codex-model-parity.py` (standard library only). The App CLI path can be supplied with `--codex-app-cli`; the PATH CLI is not assumed to be the same version.

Tests: `node --test scripts/test-models-ui.mjs`, `python3 scripts/smoke-models.py`, `python3 scripts/smoke-turns.py`, and `cargo test`.
