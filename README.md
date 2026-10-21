# Vapor-GUI

Vapor-GUI builds the graphical product executables over one shared host and service API.
`vapor-gui` opens Studio or Installer; `vapor-launcher` opens the player-facing
Launcher. The products keep their state and workflows in separate UI crates.
The precompiled `vapor-installer` remains available as the recovery path when
the graphical products are not installed.
