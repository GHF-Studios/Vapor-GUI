# Vapor IDE

Vapor IDE is the workspace authoring surface for Rust-backed content and Vapor
source workspaces. It edits original files through the Vapor Client service;
semantic objects are indexed views of those files, not a second copy of their
contents.

## Open a workspace

Open the IDE from the Vapor app chooser, then select a discovered source
workspace. The IDE loads its workspace file list through the service. Open files
appear as tabs; unsaved edits are marked with `*` and must be saved before
switching workspaces or running a managed workspace operation.

Use **Files** to browse the workspace or **Structure** to browse semantic
outlines for open files. Typing in Structure also searches Rust declarations
across the workspace when Rust Analyzer is connected; other format outlines are
available after opening those files. Selecting a workspace search result opens
its current source and selects the matching declaration in the Inspector; if
the declaration moved, disappeared, or remains ambiguous, the IDE declines to
select it. Rust files use module labels without `.rs`; `mod.rs` uses its module
path, adding crate context when duplicate paths need disambiguation.
Ordinary workspace folders sort before files. Within Cargo crate roots, files
and directories follow one alphabetical order.

Selecting a semantic object opens its Inspector entry. Rust Analyzer provides
resolved symbols and LSP navigation; a syntax pass adds recognized Bevy types,
fields, attributes, and conventional `systems.rs` / `run_conditions.rs`
classification. Selecting an indexed symbol highlights its exact source range;
the range is accepted only while its document revision is current. Vapor reports canonical-module guidance for components,
resources, events, generic types, functions, constants, and statics. Large
symbol responses are capped at 4,096 objects and 64 levels, and the IDE marks a
partial index instead of expanding an unbounded tree. Local semantic projection
is paused above 2 MiB per document; the file remains editable and Problems
explains why its outline is unavailable. TOML files receive
span-aware nodes for tables, keys, arrays, and inline tables. Recognized Vapor
manifest filenames identify their manifest role in the semantic domain, and
the Inspector annotates basic identity, version, schema, and content-reference
fields. TOML scalar values are not copied into this index. Every local adapter shares
4,096 objects and 64 nesting levels per document; capped outlines are marked in
the Inspector. The Structure view caps rendered objects at 2,048 across all open
files and asks the user to filter when the display limit is reached. Rhai also
caps its token scan at 65,536 tokens and suppresses
structural diagnostics when that scan is incomplete. XML files expose a
nested element outline and selectable attribute names; attribute and text values
stay in the source file and are not duplicated into semantic details. Both
structured adapters retain source ranges for navigation.

## Diagnostics and operations

**Problems** collects version-matched Rust Analyzer diagnostics, Vapor
convention diagnostics, and local TOML syntax errors. Selecting an anchored
diagnostic opens its file at the reported location. The Inspector also shows
diagnostics whose source ranges overlap the selected semantic object. Managed
Cargo output appears in the shared Console; compiler messages are shown as
structured diagnostics with source locations, related spans, and expandable raw
details. Diagnostic rows are virtualized so rendering cost follows the visible
range, even when the stored problem set is large.

Rust Analyzer `publishDiagnostics` notifications are normalized once by the
Vapor service into the shared protocol diagnostic shape. The IDE checks the
document revision and workspace containment before storing them. Diagnostic
text, related locations, and raw payloads have explicit bounds; oversized raw
payloads are retained as marked prefixes. Rust symbol and reference queries
also reject stale open-document snapshots, and the References panel caps its
visible results while reporting how many were omitted.

Every structured diagnostic also carries producer-independent context:
semantic domain and scope, workspace identity, optional document revision,
qualified semantic path, and originating operation ID. Problems hover details
and the shared Console expose this context; the Console can filter by domain
and scope. Older protocol records omit the context and default to an
unspecified scope, so the additive metadata does not require a wire-version
migration. Raw producer payload remains available in the detail view.

Opening a workspace does not ask Cargo to build it. Rust Analyzer is started
through the Vapor Client service with automatic checking, build scripts, and
procedural macros disabled. The IDE's **Format**, **Check**, **Test**, and
**Build** actions are explicit Vapor-managed operations and use the installed
Vapor-pinned toolchain. Format applies only if the edited document still has the
revision used for the request. Rust rename is prepared by Rust Analyzer, limited
to open files in the current workspace, and presented as a multi-file preview;
applying it updates editor buffers, and each file must still be saved to write
the edits to disk. Stale buffers, outside-workspace paths, closed files, and
file-creation/deletion edits are rejected.

## Current boundaries

- Source files remain authoritative; semantic ranges are scoped to each open
  document revision and are invalidated after edits.
- Rust semantic symbols depend on Rust Analyzer. File editing remains available
  if Rust Analyzer is unavailable.
- TOML indexing currently explains syntax and selected manifest fields; it does
  not replace Vapor's manifest validation or provide a full manifest editor.
- XML indexing is a source-navigation outline, not a typed schema editor; it
  deliberately keeps attribute and element text values only in the source.
- Rhai continues to run in the in-game Script Workbench; the IDE can inspect
  script structure, but does not execute scripts.
- **File → New Rust item** provides syntax-checked templates for components,
  resources, events, types, functions, systems, run conditions, constants, and
  statics when a matching module under the active Cargo crate's `src/` tree is
  open. It follows the Rust type, function, and constant naming conventions,
  edits only the open buffer, and leaves writing to explicit Save. Duplicate
  declarations/imports, malformed source, invalid identifiers, and files at the
  semantic indexing size limit are rejected. **New empty file** remains the
  deliberate raw-file escape.
- Create content projects in Studio's **Create** workflow. That service-owned
  operation registers the project in `Workspace.vapor.toml` and produces its
  role manifest and Cargo files together; the IDE does not create detached role
  manifests. The IDE's **File → New Vapor content** operation uses this same
  service action, then refreshes the workspace and opens the created manifest.
  More complete structured manifest editing remains future work.
