# Vapor Semantic Workbench Implementation Plan

> **Execution:** Inline implementation by the current agent; preserve existing dirty work. All Rust build, check, and test commands run through the Vapor Client managed interface, never raw Cargo.

**Goal:** Turn the Vapor IDE from a file editor into a semantic workbench where files and their internal declarations are addressable, inspectable, diagnosable, and safely editable through domain-aware operations.

**Architecture:** The Vapor Client service remains the owner of workspace access, toolchain processes, Cargo operations, and Rust Analyzer. A shared editor model consumes semantic-object and diagnostic contracts; domain adapters project Rust, Vapor content, Rhai, and structured documents into it. Original files remain authoritative; semantic identities and source ranges are refreshed from each document/version, and edits apply to source through versioned LSP edits or explicit file operations.

**Tech Stack:** Existing Rust workspace, Vapor Client protocol/service, Cargo JSON messages, Rust Analyzer LSP, `vapor_gui_core`, `vapor_ide_ui`, eframe/egui.

## Invariants

- No implicit Cargo builds on workspace open, file discovery, or IDE startup.
- Service owns filesystem and process access; UI fronts consume typed service events.
- Native structured payloads and raw stdout/stderr are both retained; parsing may not discard tool output.
- A source file is authoritative; semantic nodes are indexed projections, never a competing editable copy.
- Semantic references are scoped to a workspace and document version; stale ranges cannot silently apply edits.
- Rust conventions are dogmatic defaults with explicit escape paths and actionable diagnostics.
- Ordinary folders sort before files; Rust crate entries sort together alphanumerically.
- No broad formatting of unrelated files; no user data, caches, or Workshop state deletion.

## Review Focus

- A file changes after a symbol index is built: invalidate/rebuild its nodes and reject stale edits.
- Rust Analyzer is unavailable or returns partial symbols: preserve file editing and surface capability status.
- Cargo or build-script output is not JSON: keep it as raw records and continue consuming the stream.
- The same issue arrives from live LSP and Cargo: link/deduplicate views without losing either provenance.
- Very large workspaces: keep discovery, symbol trees, diagnostics, and console rendering bounded/incremental.

## Work Units

### 1. Restore managed verification

Trace the malformed Client service response from request through provider output to the UI. Add bounded frame diagnostics that expose the offending prefix safely, reproduce through Vapor-managed `check`/`test`, and fix the writer/stream ownership defect at its source. Establish a passing managed check/test path before relying on build feedback.

### 2. Define shared semantic and diagnostic contracts

Add versioned, serializable workspace semantic nodes (identity, domain, kind, display path, source range, document revision, parent/children, detail, capabilities) and diagnostics (producer, domain/scope, severity, code, message, source/object anchor, related locations, fixes, operation/document identity, original payload). Preserve compatibility defaults for new fields and unit-test serialization, bounds, stale revisions, and identity behavior.

### 3. Normalize service-owned tool events

Parse Cargo `--message-format=json` records into typed compiler diagnostics and artifact/build events while preserving all raw output. Forward Rust Analyzer diagnostics, document/workspace symbols, and code actions as structured service events with workspace and document version. Keep standard output from arbitrary tools as ordinary console records. Add provenance and operation correlation so one issue can be opened from console, Problems, file, or semantic inspector.

### 4. Build the semantic workspace index

Create a shared, incremental index keyed by workspace and canonical crate/module/file identity. Rust Analyzer supplies resolved symbols and references; syntax-level metadata classifies Vapor conventions such as `#[derive(Component)]` without reimplementing name resolution. Index declaration children such as component fields as selectable semantic objects. Rebuild only affected documents; do not load every dependency source by default.

### 5. Make navigation semantic

Upgrade the workspace tree with the required ordering rules and semantic labels: hide `.rs`, render `mod.rs` by shortest unique module path, reveal fuller paths only on collision, and allow navigation/search by crate, module, type, component, field, system, and diagnostic. Keep files available as source documents and permit expert raw-file creation.

### 6. Add first deep Rust inspector and safe actions

Selecting a Rust symbol opens a structured inspector. For components, show fields/attributes/derives, declaration location, direct references and registrations available from Rust Analyzer, plus the exact convention status. Use LSP versioned edits for local refactors; preview multi-document `WorkspaceEdit`s before applying; update open buffers and symbol index atomically. Add regression cases for nested modules, duplicate `mod.rs`, macro/derive components, unsaved edits, and external references.

### 7. Unify diagnostics and console presentation

Render typed diagnostics in a virtualized Problems view and console with domain/producer/scope filters, related locations, raw details, and safe fix actions. Provide a selected-object diagnostic view without duplicating storage. Retain CLI-readable equivalents through the same event model.

### 8. Connect domain adapters and authoring flows

Apply the shared contract to Vapor content/manifests and Rhai/script workbench, then TOML/XML structured inspectors. Add template-based semantic creation for canonical modules/content while preserving explicit raw-file escape. Domain adapters own vocabulary and actions; shared machinery owns indexing, identity, navigation, diagnostics, history, and operation dispatch.

### 9. Verify and package

Run focused managed tests per work unit, then managed validation for Vapor, Vapor-GUI, and affected workspaces. Exercise no-build-on-open, Cargo JSON plus raw fallback, LSP symbol/diagnostic navigation, stale edit rejection, and cross-frontend event parity. Locally deploy only if installation/runtime validation materially improves confidence. Report verified scope and remaining adapters honestly; do not claim full IDE parity from the Rust vertical slice alone.
