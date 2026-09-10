# ChatGPT Web Multi-Model Routing Implementation Plan

> **For agentic workers:** REQUIRED EXECUTION MODE: use Jcode's native swarm task graph or `executing-plans` task-by-task. Do not use `subagent-driven-development`.

**Goal:** Preserve `gpt-5.6-pro[web]`, add `gpt-6-astra[web]`, and make the browser-backed ChatGPT transport select and verify each model from one descriptor registry.

**Architecture:** `jcode-provider-core` owns an immutable registry describing every ChatGPT Web route. Provider catalogs and the OpenAI runtime use registry lookup helpers instead of comparing one constant. `chatgpt_web.rs` resolves the requested descriptor once, then derives navigation, picker verification, status text, response-slug validation, and diagnostics from it.

**Tech Stack:** Rust, Tokio, serde_json, Jcode provider-core/base/OpenAI runtime crates, Firefox Agent Bridge, Cargo tests.

## Global Constraints

- Preserve both web routes: `gpt-5.6-pro[web]` and `gpt-6-astra[web]`.
- Map `gpt-5.6-pro[web]` to query/response slug `gpt-5-6-pro` and picker label `5.6 Pro`.
- Map `gpt-6-astra[web]` to query/response slug `gpt-6-pro` and picker label `6 Pro`.
- Keep API/OAuth `gpt-6-astra` distinct from browser `gpt-6-astra[web]`.
- Never send the Jcode system prompt unless exact model verification and Temporary Chat verification both pass.
- Keep the isolated Firefox fork and secure cleanup behavior.
- Do not modify the unrelated dirty files in `crates/jcode-app-core/src/agent_tests/concurrency_construction.rs`, `crates/jcode-app-core/src/server/client_actions_tests.rs`, `crates/jcode-tui/src/tui/session_picker.rs`, or `crates/jcode-tui/src/tui/session_picker_tests.rs`.
- Do not switch `current`, `stable`, or `shared-server` during implementation.
- Commit each independently testable task separately.

---

### Task 1: Add the ChatGPT Web model descriptor registry

**Files:**
- Modify: `crates/jcode-provider-core/src/models.rs:29-120`
- Modify: `crates/jcode-provider-core/src/lib.rs:35-50`
- Test: `crates/jcode-provider-core/src/models.rs` inline tests

**Interfaces:**
- Produces: `ChatGptWebModelDescriptor` with fields `model_id`, `query_slug`, `picker_label`, `response_slug`, and `display_label`.
- Produces: `CHATGPT_WEB_MODEL`, `CHATGPT_WEB_ASTRA_MODEL`, `CHATGPT_WEB_MODELS`.
- Produces: `chatgpt_web_model_descriptor(model: &str) -> Option<&'static ChatGptWebModelDescriptor>`.
- Produces: `is_chatgpt_web_model(model: &str) -> bool`.
- Consumed by: Tasks 2 through 4.

- [ ] **Step 1: Write failing registry tests**

Add tests equivalent to:

```rust
#[test]
fn chatgpt_web_registry_maps_supported_routes() {
    let legacy = chatgpt_web_model_descriptor("gpt-5.6-pro[web]").unwrap();
    assert_eq!(legacy.query_slug, "gpt-5-6-pro");
    assert_eq!(legacy.picker_label, "5.6 Pro");
    assert_eq!(legacy.response_slug, "gpt-5-6-pro");

    let astra = chatgpt_web_model_descriptor("gpt-6-astra[web]").unwrap();
    assert_eq!(astra.query_slug, "gpt-6-pro");
    assert_eq!(astra.picker_label, "6 Pro");
    assert_eq!(astra.response_slug, "gpt-6-pro");
}

#[test]
fn chatgpt_web_registry_rejects_unknown_routes() {
    assert!(chatgpt_web_model_descriptor("gpt-6-astra").is_none());
    assert!(chatgpt_web_model_descriptor("gpt-7-pro[web]").is_none());
    assert!(!is_chatgpt_web_model("gpt-6-astra"));
}
```

Update the complete OpenAI family test to require `gpt-6-astra[web]` in `ALL_OPENAI_MODELS`.

- [ ] **Step 2: Run the tests and verify RED**

Run:

```bash
/Volumes/macOS/Users/kleksar/.cargo/bin/cargo test -p jcode-provider-core chatgpt_web_registry --lib
```

Expected: compile failure because the descriptor type, Astra web constant, registry, and lookup helpers do not exist.

- [ ] **Step 3: Implement the minimal registry**

Add:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChatGptWebModelDescriptor {
    pub model_id: &'static str,
    pub query_slug: &'static str,
    pub picker_label: &'static str,
    pub response_slug: &'static str,
    pub display_label: &'static str,
}

pub const CHATGPT_WEB_MODEL: &str = "gpt-5.6-pro[web]";
pub const CHATGPT_WEB_ASTRA_MODEL: &str = "gpt-6-astra[web]";

pub const CHATGPT_WEB_MODELS: &[ChatGptWebModelDescriptor] = &[
    ChatGptWebModelDescriptor {
        model_id: CHATGPT_WEB_MODEL,
        query_slug: "gpt-5-6-pro",
        picker_label: "5.6 Pro",
        response_slug: "gpt-5-6-pro",
        display_label: "GPT-5.6 Pro",
    },
    ChatGptWebModelDescriptor {
        model_id: CHATGPT_WEB_ASTRA_MODEL,
        query_slug: "gpt-6-pro",
        picker_label: "6 Pro",
        response_slug: "gpt-6-pro",
        display_label: "GPT-6 Astra",
    },
];

pub fn chatgpt_web_model_descriptor(
    model: &str,
) -> Option<&'static ChatGptWebModelDescriptor> {
    let model = model.trim();
    CHATGPT_WEB_MODELS
        .iter()
        .find(|descriptor| descriptor.model_id == model)
}

pub fn is_chatgpt_web_model(model: &str) -> bool {
    chatgpt_web_model_descriptor(model).is_some()
}
```

Insert `CHATGPT_WEB_ASTRA_MODEL` in `ALL_OPENAI_MODELS` adjacent to the legacy web route. Re-export the new type, constants, and helpers from `jcode-provider-core/src/lib.rs`.

- [ ] **Step 4: Run focused and crate tests**

Run:

```bash
/Volumes/macOS/Users/kleksar/.cargo/bin/cargo test -p jcode-provider-core chatgpt_web_registry --lib
/Volumes/macOS/Users/kleksar/.cargo/bin/cargo test -p jcode-provider-core openai_catalog_exposes --lib
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/jcode-provider-core/src/models.rs crates/jcode-provider-core/src/lib.rs
git commit -m "feat(provider): describe ChatGPT web model routes"
```

---

### Task 2: Expose every web descriptor through provider catalogs

**Files:**
- Modify: `crates/jcode-base/src/provider/mod.rs`
- Modify: `crates/jcode-base/src/provider/route_builders.rs:152-161`
- Modify: `crates/jcode-base/src/provider/catalog_routes.rs:1-45,375-440`
- Modify: `crates/jcode-base/src/provider/models.rs:690-720,994-1002`
- Test: `crates/jcode-base/src/provider/tests.rs:423-487`
- Test: `crates/jcode-base/src/provider/tests/catalog_subscription.rs:54-81`

**Interfaces:**
- Consumes: Task 1 registry and lookup helpers.
- Changes: `build_chatgpt_web_route(model: &str) -> ModelRoute`.
- Produces: picker, full catalog, known-model list, and browser-session availability for both web ids.

- [ ] **Step 1: Write failing route coverage tests**

Change route tests to assert exactly one `chatgpt-web` route for each registry entry:

```rust
for descriptor in jcode_provider_core::CHATGPT_WEB_MODELS {
    let route = routes
        .iter()
        .find(|route| route.model == descriptor.model_id)
        .expect("missing ChatGPT web route");
    assert_eq!(route.provider, "OpenAI");
    assert_eq!(route.api_method, "chatgpt-web");
    assert!(route.available);
}
```

Update catalog replacement tests so live API catalog hydration still retains both web models.

Add availability assertions:

```rust
for descriptor in jcode_provider_core::CHATGPT_WEB_MODELS {
    let availability = model_availability_for_account(descriptor.model_id);
    assert_eq!(availability.source, "browser-session");
}
```

- [ ] **Step 2: Run tests and verify RED**

Run:

```bash
/Volumes/macOS/Users/kleksar/.cargo/bin/cargo test -p jcode-base openai_model_routes_cover --lib
/Volumes/macOS/Users/kleksar/.cargo/bin/cargo test -p jcode-base openai_live_catalog_replaces --lib
```

Expected: Astra web route is absent and the singular route builder cannot accept a model id.

- [ ] **Step 3: Generalize route construction and catalog checks**

Implement:

```rust
pub fn build_chatgpt_web_route(model: &str) -> ModelRoute {
    debug_assert!(jcode_provider_core::is_chatgpt_web_model(model));
    ModelRoute {
        model: model.to_string(),
        provider: "OpenAI".to_string(),
        api_method: "chatgpt-web".to_string(),
        available: true,
        detail: "logged-in Firefox ChatGPT session".to_string(),
        cheapness: None,
    }
}
```

Replace `model == CHATGPT_WEB_MODEL` with `is_chatgpt_web_model(&model)` in simplified and full route construction. Pass the concrete model id to the builder.

Ensure `known_openai_model_ids()` inserts every `descriptor.model_id` from `CHATGPT_WEB_MODELS`, and make `model_availability_for_account()` classify every registered web model as `browser-session`.

- [ ] **Step 4: Run focused provider-base tests**

Run:

```bash
/Volumes/macOS/Users/kleksar/.cargo/bin/cargo test -p jcode-base openai_model_routes_cover --lib
/Volumes/macOS/Users/kleksar/.cargo/bin/cargo test -p jcode-base openai_live_catalog_replaces --lib
/Volumes/macOS/Users/kleksar/.cargo/bin/cargo test -p jcode-base model_availability --lib
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/jcode-base/src/provider/mod.rs \
  crates/jcode-base/src/provider/route_builders.rs \
  crates/jcode-base/src/provider/catalog_routes.rs \
  crates/jcode-base/src/provider/models.rs \
  crates/jcode-base/src/provider/tests.rs \
  crates/jcode-base/src/provider/tests/catalog_subscription.rs
git commit -m "feat(provider): list all ChatGPT web routes"
```

---

### Task 3: Make OpenAI runtime switching registry-aware

**Files:**
- Modify: `crates/jcode-provider-openai-runtime/src/lib.rs:41-50,778-797`
- Modify: `crates/jcode-provider-openai-runtime/src/openai_provider_impl.rs:743-846`
- Test: `crates/jcode-provider-openai-runtime/src/openai_tests/models_state.rs:64-128`

**Interfaces:**
- Consumes: Task 1 registry.
- Produces: browser-only and credentialed runtimes that expose and switch between both web ids.
- Preserves: legacy browser-only default `gpt-5.6-pro[web]` to avoid an implicit behavior change.

- [ ] **Step 1: Write failing runtime tests**

Replace singular assertions with registry-wide assertions:

```rust
#[test]
fn test_chatgpt_web_models_bypass_live_api_catalog() {
    // existing setup
    for descriptor in jcode_provider_core::CHATGPT_WEB_MODELS {
        provider.set_model(descriptor.model_id).unwrap();
        assert_eq!(provider.model(), descriptor.model_id);
        assert_eq!(provider.transport().as_deref(), Some("browser"));
    }
}

#[test]
fn test_chatgpt_browser_only_runtime_exposes_all_web_models() {
    let provider = OpenAIProvider::new_browser_only();
    assert_eq!(provider.model(), jcode_provider_core::CHATGPT_WEB_MODEL);
    assert_eq!(
        provider.available_models(),
        jcode_provider_core::CHATGPT_WEB_MODELS
            .iter()
            .map(|descriptor| descriptor.model_id)
            .collect::<Vec<_>>()
    );
    provider
        .set_model(jcode_provider_core::CHATGPT_WEB_ASTRA_MODEL)
        .unwrap();
}
```

Add an environment-override test for whitespace around `gpt-6-astra[web]`.

- [ ] **Step 2: Run tests and verify RED**

Run:

```bash
/Volumes/macOS/Users/kleksar/.cargo/bin/cargo test -p jcode-provider-openai-runtime chatgpt_web --lib
```

Expected: the browser-only runtime exposes only the legacy route.

- [ ] **Step 3: Replace singular runtime assumptions**

Delegate the local `is_chatgpt_web_model()` helper to `jcode_provider_core::is_chatgpt_web_model()`.

For browser-only errors, list supported ids instead of naming one:

```rust
let supported = jcode_provider_core::CHATGPT_WEB_MODELS
    .iter()
    .map(|descriptor| descriptor.model_id)
    .collect::<Vec<_>>()
    .join(", ");
```

Return all registry model ids from `available_models()` and `available_models_for_switching()`. For credentialed runtimes, insert any missing web route while retaining stable registry order.

- [ ] **Step 4: Run focused runtime tests**

Run:

```bash
/Volumes/macOS/Users/kleksar/.cargo/bin/cargo test -p jcode-provider-openai-runtime chatgpt_web --lib
/Volumes/macOS/Users/kleksar/.cargo/bin/cargo test -p jcode-provider-openai-runtime switching_models --lib
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/jcode-provider-openai-runtime/src/lib.rs \
  crates/jcode-provider-openai-runtime/src/openai_provider_impl.rs \
  crates/jcode-provider-openai-runtime/src/openai_tests/models_state.rs
git commit -m "feat(openai): switch between ChatGPT web models"
```

---

### Task 4: Drive browser behavior from the selected descriptor

**Files:**
- Modify: `crates/jcode-provider-openai-runtime/src/chatgpt_web.rs:11-530`
- Test: `crates/jcode-provider-openai-runtime/src/chatgpt_web.rs:792-915`

**Interfaces:**
- Consumes: `chatgpt_web_model_descriptor(model)` from Task 1.
- Produces: `chatgpt_web_url(descriptor) -> String`.
- Produces: `normalize_picker_label(raw: &str) -> String`.
- Produces: `page_verification_ready(verification: &Value, descriptor: &ChatGptWebModelDescriptor) -> bool`.
- Produces: `response_model_matches(slug: &str, descriptor: &ChatGptWebModelDescriptor) -> bool`.

- [ ] **Step 1: Write failing pure-function tests**

Add:

```rust
#[test]
fn chatgpt_web_urls_use_registered_query_slugs() {
    let legacy = chatgpt_web_model_descriptor(CHATGPT_WEB_MODEL).unwrap();
    assert_eq!(
        chatgpt_web_url(legacy),
        "https://chatgpt.com/?model=gpt-5-6-pro&temporary-chat=true"
    );
    let astra = chatgpt_web_model_descriptor(CHATGPT_WEB_ASTRA_MODEL).unwrap();
    assert_eq!(
        chatgpt_web_url(astra),
        "https://chatgpt.com/?model=gpt-6-pro&temporary-chat=true"
    );
}

#[test]
fn picker_labels_collapse_chatgpt_whitespace() {
    assert_eq!(normalize_picker_label("6\nPro"), "6 Pro");
    assert_eq!(normalize_picker_label("  5.6   Pro "), "5.6 Pro");
}

#[test]
fn page_verification_requires_descriptor_model_and_temporary_chat() {
    let astra = chatgpt_web_model_descriptor(CHATGPT_WEB_ASTRA_MODEL).unwrap();
    assert!(page_verification_ready(
        &json!({"model": "6\nPro", "temporary": true}),
        astra
    ));
    assert!(!page_verification_ready(
        &json!({"model": "5.6\nPro", "temporary": true}),
        astra
    ));
}

#[test]
fn response_slug_must_match_selected_web_model() {
    let astra = chatgpt_web_model_descriptor(CHATGPT_WEB_ASTRA_MODEL).unwrap();
    assert!(response_model_matches("gpt-6-pro", astra));
    assert!(!response_model_matches("gpt-5-6-pro", astra));
}
```

- [ ] **Step 2: Run tests and verify RED**

Run:

```bash
/Volumes/macOS/Users/kleksar/.cargo/bin/cargo test -p jcode-provider-openai-runtime chatgpt_web_urls --lib
/Volumes/macOS/Users/kleksar/.cargo/bin/cargo test -p jcode-provider-openai-runtime picker_labels --lib
/Volumes/macOS/Users/kleksar/.cargo/bin/cargo test -p jcode-provider-openai-runtime page_verification_requires --lib
```

Expected: helper functions and descriptor-driven signatures do not exist.

- [ ] **Step 3: Resolve and carry the descriptor through one turn**

At the start of `complete()` or `run_turn()`, resolve:

```rust
let descriptor = jcode_provider_core::chatgpt_web_model_descriptor(model)
    .ok_or_else(|| anyhow::anyhow!("Unsupported ChatGPT web model: {model}"))?;
```

Use `descriptor.display_label` in connection status and errors. Pass the descriptor to navigation, page preparation, response polling, and response emission diagnostics.

- [ ] **Step 4: Generalize navigation and editor readiness**

Replace the constant URL with:

```rust
fn chatgpt_web_url(descriptor: &ChatGptWebModelDescriptor) -> String {
    format!(
        "https://chatgpt.com/?model={}&temporary-chat=true",
        descriptor.query_slug
    )
}
```

Use:

```rust
const EDITOR_SELECTOR: &str = "#prompt-textarea[contenteditable=true]";
const EDITOR_FALLBACK_SELECTOR: &str =
    "[contenteditable=true][aria-label='Chat with ChatGPT']";
```

`wait_for_editor()` first waits for `EDITOR_SELECTOR`. If that bridge command fails, retry the semantic fallback before returning the existing login/workspace diagnostic. Keep all prompt fill and fingerprint logic scoped to the resolved visible editor.

- [ ] **Step 5: Generalize picker and Temporary Chat verification**

In browser evaluation, select only composer-local menu buttons and normalize whitespace in Rust:

```javascript
const model = Array.from(
  document.querySelectorAll('form[data-type="unified-composer"] button[aria-haspopup="menu"]')
).map(button => button.innerText || '').find(text => /\bPro\b/.test(text)) || '';
```

Keep the existing onboarding refusal. Detect Temporary Chat using the explicit off button first and the current explanatory text only as a fallback.

Require:

```rust
normalize_picker_label(selected_model) == descriptor.picker_label
```

- [ ] **Step 6: Generalize response polling and diagnostics**

Keep the current latest assistant message selector. Validate:

```rust
response_model_matches(upstream_model, descriptor)
```

Every timeout, status update, unknown-tool, malformed-envelope, and cleanup error must name `descriptor.display_label` rather than GPT-5.6 Pro.

- [ ] **Step 7: Run all focused web transport tests**

Run:

```bash
/Volumes/macOS/Users/kleksar/.cargo/bin/cargo test -p jcode-provider-openai-runtime chatgpt_web --lib
/Volumes/macOS/Users/kleksar/.cargo/bin/cargo test -p jcode-provider-openai-runtime tool_call_parser --lib
/Volumes/macOS/Users/kleksar/.cargo/bin/cargo test -p jcode-provider-openai-runtime web_prompt --lib
```

Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/jcode-provider-openai-runtime/src/chatgpt_web.rs
git commit -m "feat(openai): route ChatGPT web turns by descriptor"
```

---

### Task 5: Verify picker presentation and complete regression coverage

**Files:**
- Modify: `crates/jcode-tui/src/tui/app/helpers/model_names.rs:18-65,354-437`
- Modify only if test failures require it: provider catalog test files from Tasks 1 through 3

**Interfaces:**
- Consumes: new model id `gpt-6-astra[web]`.
- Produces: stable user-facing name `GPT-6 Astra (web)` without hiding the exact route id in copyable contexts.

- [ ] **Step 1: Add the pretty-name regression test**

```rust
assert_eq!(
    pretty_model_display_name("gpt-6-astra[web]"),
    "GPT-6 Astra (web)"
);
assert_eq!(
    pretty_known_model_family("gpt-6-astra[web]").as_deref(),
    Some("GPT-6 Astra (web)")
);
```

- [ ] **Step 2: Run the test**

Run:

```bash
/Volumes/macOS/Users/kleksar/.cargo/bin/cargo test -p jcode-tui pretty_model_display_name_formats_common_models --lib
/Volumes/macOS/Users/kleksar/.cargo/bin/cargo test -p jcode-tui pretty_known_model_family_gates --lib
```

Expected: PASS with existing generic formatting, or RED exposing the minimal formatting gap.

- [ ] **Step 3: Apply only the minimal formatting correction if needed**

Do not add a special-case display map if the generic GPT and bracket-suffix logic already returns `GPT-6 Astra (web)`. If it fails, adjust only the shared versioned-family tokenization needed for this id and keep existing snapshots unchanged.

- [ ] **Step 4: Run provider and TUI regression set**

```bash
/Volumes/macOS/Users/kleksar/.cargo/bin/cargo test -p jcode-provider-core --lib
/Volumes/macOS/Users/kleksar/.cargo/bin/cargo test -p jcode-base provider::tests --lib
/Volumes/macOS/Users/kleksar/.cargo/bin/cargo test -p jcode-provider-openai-runtime --lib
/Volumes/macOS/Users/kleksar/.cargo/bin/cargo test -p jcode-tui pretty_model --lib
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/jcode-tui/src/tui/app/helpers/model_names.rs
git commit -m "test(tui): cover Astra ChatGPT web display name"
```

If Step 3 required no implementation change, use the same commit message for the test-only commit.

---

### Task 6: Build an immutable candidate and run live browser acceptance

**Files:**
- Create: `~/.jcode/builds/versions/<commit>-chatgpt-web-multimodel/jcode` as an immutable local artifact
- Create: private machine-local deployment manifest and rollback note outside the repository
- Do not modify channel symlinks

**Interfaces:**
- Consumes: all implementation tasks.
- Produces: an isolated candidate with verified catalog and live ChatGPT Web behavior.

- [ ] **Step 1: Verify repository hygiene and diff scope**

Run:

```bash
git status --short
git diff --check HEAD~4..HEAD
git diff --name-only backup/pre-upstream-20260910-1640..HEAD
```

Confirm the four unrelated dirty files remain unstaged and unchanged by this feature.

- [ ] **Step 2: Run compile verification**

```bash
/Volumes/macOS/Users/kleksar/.cargo/bin/cargo check -p jcode
```

Expected: PASS. Existing unrelated warnings may remain, but no new warning may originate from changed ChatGPT Web code.

- [ ] **Step 3: Build a clean immutable artifact**

Create or reuse an isolated clean checkout at the exact feature head. Run:

```bash
/Volumes/macOS/Users/kleksar/.cargo/bin/cargo build --release --bin jcode
```

Copy the binary into a new immutable version directory without changing `~/.jcode/builds/current`, `stable`, or `shared-server`. Record `jcode version --json` and SHA-256.

- [ ] **Step 4: Verify candidate catalog**

Run:

```bash
<CANDIDATE> model list --json
```

Assert the JSON includes separate `chatgpt-web` routes for `gpt-5.6-pro[web]` and `gpt-6-astra[web]`, alongside API/OAuth `gpt-6-astra`.

- [ ] **Step 5: Run isolated live smoke for GPT-5.6 Pro Web**

Start the candidate on an isolated socket and send a no-tool prompt through `gpt-5.6-pro[web]`:

```text
Reply exactly: WEB_56_OK
```

Acceptance:

- selected picker label is `5.6 Pro`;
- Temporary Chat is active;
- response is `WEB_56_OK`;
- response slug is `gpt-5-6-pro`;
- owned fork closes.

- [ ] **Step 6: Run isolated live smoke for GPT-6 Astra Web**

Send through `gpt-6-astra[web]`:

```text
Reply exactly: WEB_6_OK
```

Acceptance:

- selected picker label is `6 Pro`;
- Temporary Chat is active;
- response is `WEB_6_OK`;
- response slug is `gpt-6-pro`;
- owned fork closes.

- [ ] **Step 7: Run one tool-call round trip**

Expose only the `read` tool and ask the web route to read a small known text fixture through the mandatory Jcode tool envelope. Confirm tool use start/input/end events parse, the tool result is returned in the next browser turn, and the final answer matches the fixture. Run against Astra Web first; repeat on GPT-5.6 Web only if shared behavior is not already proven by the common transport and unit suite.

- [ ] **Step 8: Record deployment boundary**

Record candidate identity, prior client and daemon identities, checksums, commands, live results, and rollback commands. Stop before any channel or daemon switch and ask for explicit deployment scope.

---

## Final self-review checklist

- Every design requirement maps to a task: registry (Task 1), catalogs (Task 2), runtime switching (Task 3), browser semantics and errors (Task 4), presentation (Task 5), live acceptance and immutable deployment boundary (Task 6).
- All model ids, query slugs, picker labels, and response slugs are consistent across tasks.
- No task modifies the four unrelated dirty files.
- No task switches client, daemon, stable, current, or shared-server without a later explicit approval.
