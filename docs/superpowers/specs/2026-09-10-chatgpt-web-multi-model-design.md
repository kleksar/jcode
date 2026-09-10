# ChatGPT Web multi-model routing design

Date: 2026-09-10
Status: approved direction, pending implementation

## Goal

Generalize Jcode's browser-backed ChatGPT provider so it can expose and verify more than one ChatGPT Web model. Preserve `gpt-5.6-pro[web]` and add `gpt-6-astra[web]`, which maps to the ChatGPT UI model `6 Pro` and upstream response slug `gpt-6-pro`.

## Verified current state

- Official upstream at `ce4e789564c2dfa5e3edbddf34eb69a8ba488533` exposes only `gpt-5.6-pro[web]`.
- The local integration branch includes upstream through merge commit `9c6dfc7f8b792cee7ab0f3aa4c12c8d538c95b0c`.
- The current provider is single-model and hard-codes the URL, picker label, response slug, status text, and error messages for GPT-5.6 Pro.
- Live ChatGPT Web on the user's Pro account exposes `6 Pro`.
- `https://chatgpt.com/?model=gpt-5-6-pro&temporary-chat=true` selects `5.6 Pro`.
- `https://chatgpt.com/?model=gpt-6-pro&temporary-chat=true` selects `6 Pro`.
- A live Temporary Chat response from `6 Pro` returned `data-message-model-slug="gpt-6-pro"`.
- The current visible editor is `#prompt-textarea[contenteditable="true"]`; a hidden fallback `<textarea name="prompt-textarea">` also exists.

## Architecture

Introduce a small immutable descriptor for each supported ChatGPT Web model:

- Jcode model id
- ChatGPT query slug
- normalized picker label
- accepted response model slugs
- human-readable display label

The registry initially contains:

| Jcode id | Query slug | Picker label | Response slug |
|---|---|---|---|
| `gpt-5.6-pro[web]` | `gpt-5-6-pro` | `5.6 Pro` | `gpt-5-6-pro` |
| `gpt-6-astra[web]` | `gpt-6-pro` | `6 Pro` | `gpt-6-pro` |

All web-provider behavior resolves through this registry. Unknown `[web]` models fail before opening a browser tab.

## Catalog and routing

- Replace the singular `CHATGPT_WEB_MODEL` assumption with `CHATGPT_WEB_MODELS` plus lookup helpers.
- Keep the existing constant as a compatibility alias if that reduces churn for callers and tests.
- Add both routes to the picker with provider `OpenAI`, method `chatgpt-web`, and browser-session availability detail.
- Browser-only OpenAI runtimes expose both web models.
- Normal OpenAI runtimes can switch to either web model without consulting the API/OAuth catalog.
- `gpt-6-astra` and `gpt-6-astra[web]` remain distinct routes and transports.

## Browser flow

1. Resolve the requested model descriptor.
2. Fork the active Firefox tab using the existing isolated-tab mechanism.
3. Navigate to `https://chatgpt.com/?model=<query-slug>&temporary-chat=true`.
4. Wait for the stable visible editor selector `#prompt-textarea[contenteditable="true"]`.
5. Preserve the existing refusal to auto-accept workspace migration or onboarding.
6. Dismiss only the recognized Temporary Chat explainer.
7. Normalize the model button text by collapsing whitespace, then require an exact descriptor picker label.
8. Require Temporary Chat before inserting any Jcode system prompt.
9. Fill and verify the rich-text editor using the stable editor selector and existing UTF-16 fingerprint.
10. Submit and poll the newest assistant turn.
11. Require the response's `data-message-model-slug` to match the descriptor's accepted response slug.
12. Securely close the owned fork as today.

## DOM compatibility

Use narrowly-scoped selector fallbacks only where they preserve the same semantic element:

- Preferred editor: `#prompt-textarea[contenteditable="true"]`.
- Compatibility editor: `[contenteditable="true"][aria-label="Chat with ChatGPT"]`.
- Model button: a composer-local button whose normalized text matches a registered picker label.
- Assistant response: the latest visible `[data-message-author-role="assistant"]`, with the existing section and markdown extraction as fallbacks.

Do not select models by ordinal menu position or broad text search outside the composer.

## Errors and diagnostics

All status and error text names the selected descriptor instead of GPT-5.6 Pro. Fail closed when:

- the requested web model is unknown;
- Firefox or the bridge is unavailable;
- the account is signed out;
- onboarding requires a user decision;
- the exact picker label is not selected;
- Temporary Chat is not active;
- prompt fingerprinting fails;
- the response model slug differs from the descriptor;
- the owned browser fork cannot be securely cleared or closed.

## Testing

### Unit and catalog tests

- Registry lookup accepts both supported ids and rejects unknown web ids.
- URL construction maps each id to its verified query slug.
- Picker-label normalization converts `"6\nPro"` to `"6 Pro"`.
- Response-slug validation accepts only the descriptor's slugs.
- Catalog, route builder, account availability, browser-only runtime, model switching, and pretty-name tests cover both web models.
- Existing GPT-5.6 Web tool-envelope and prompt-integrity tests remain green.

### Live acceptance

Using logged-in Firefox and Temporary Chat:

1. `gpt-5.6-pro[web]` returns an exact smoke token and reports `gpt-5-6-pro`.
2. `gpt-6-astra[web]` returns an exact smoke token and reports `gpt-6-pro`.
3. One minimal Jcode tool-call round trip succeeds for each route, or the limitation is recorded if ChatGPT Web behavior prevents deterministic tool emission.
4. Owned browser forks close and the source tab remains intact.

## Deployment boundary

- Implement and test on top of `custom/ui-stable` without modifying the four unrelated dirty session-picker/test files.
- Build a new immutable candidate artifact.
- Do not switch `current`, `stable`, or `shared-server` during implementation.
- A client or daemon switch requires its own explicit scope, rollback record, identity verification, and applicable visual acceptance under `docs/LOCAL_DEVELOPMENT.md`.
