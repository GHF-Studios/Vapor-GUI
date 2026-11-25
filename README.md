# Vapor-GUI

Vapor-GUI builds the graphical product executables over one shared host and service API.
`vapor-gui` opens the small product chooser. `vapor-launcher`, `vapor-studio`,
and `vapor-ide` are separate player, creator, and Rust-workspace applications;
they share the same eframe host and connect to the same Vapor Client service.
`vapor-installer-gui` provides graphical setup and recovery. The IDE edits Rust
workspaces through the service-owned workspace file API; in-game Rhai authoring
remains in the game's Script Workbench. Rust Analyzer runs through the shared
Vapor Client service with the pinned toolchain. The IDE synchronizes open Rust
files and provides language diagnostics, definition navigation, hover, and
completion; broader LSP features and external dependency source editing remain
under development.
See [`docs/ide.md`](docs/ide.md) for the current IDE workflow and its semantic
indexing/build boundaries.

`vapor-setup` opens the standalone installer UI when launched without a
subcommand. `vapor-setup install [--channel stable]` installs and activates the
latest signed release without opening a window, using the same verified
acquisition path. This supports managed bootstrap automation while keeping
the downloadable setup executable as the sole entry point.
The precompiled `vapor-installer` remains available as the recovery path when
the graphical products are not installed.
