# Vapor-GUI

Vapor-GUI is the graphical host process. It selects and runs GUI products such
as Studio and Installer while keeping product behavior in their own modules.
The executable assembles `vapor_studio_ui` and `vapor_installer_ui` without
owning either product's state or workflows.
The Steam-facing binary is `vapor-gui`; the precompiled `vapor-installer`
remains available as the recovery path when this host is not installed.
