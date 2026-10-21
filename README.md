# Vapor-GUI

Vapor-GUI builds the graphical product executables over one shared host and service API.
`vapor-gui` hosts the Studio, IDE, and Installer products; `vapor-launcher`
opens the player-facing Launcher. These products keep their workflows in
separate UI crates and connect to the same Vapor Client service. The IDE edits
Rust workspaces through the service-owned workspace file API; in-game Rhai
authoring remains in the game's Script Workbench. Rust Analyzer integration is
not connected yet.
The precompiled `vapor-installer` remains available as the recovery path when
the graphical products are not installed.
