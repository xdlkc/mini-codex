//! Shared desktop HTML and optional native-window entry point.

pub fn app_html() -> String {
    include_str!("gui_app.html")
        .replace("/* DESKTOP_STYLES */", include_str!("gui_desktop.css"))
        .replace(
            "// DESKTOP_SCRIPT",
            &[
                include_str!("gui_native_windows.js"),
                include_str!("gui_desktop.js"),
                include_str!("gui_scheduled.js"),
                include_str!("gui_account.js"),
                include_str!("gui_workspaces.js"),
                include_str!("gui_shell.js"),
                include_str!("gui_files.js"),
                include_str!("gui_voice.js"),
                include_str!("gui_models.js"),
                include_str!("gui_images.js"),
                include_str!("gui_remote.js"),
                include_str!("gui_documents.js"),
                include_str!("gui_document_tools.js"),
                include_str!("gui_popovers.js"),
                include_str!("gui_search.js"),
                include_str!("gui_native_controls.js"),
                include_str!("gui_work_local.js"),
            ]
            .join("\n"),
        )
        .replace(
            "/* SCHEDULED_STYLES */",
            &[
                include_str!("gui_scheduled.css"),
                include_str!("gui_account.css"),
                include_str!("gui_workspaces.css"),
                include_str!("gui_shell.css"),
                include_str!("gui_files.css"),
                include_str!("gui_voice.css"),
                include_str!("gui_models.css"),
                include_str!("gui_images.css"),
                include_str!("gui_remote.css"),
                include_str!("gui_documents.css"),
                include_str!("gui_document_tools.css"),
                include_str!("gui_search.css"),
                include_str!("gui_native_windows.css"),
                include_str!("gui_work_local.css"),
            ]
            .join("\n"),
        )
        .replace("// MARKDOWN_VENDOR", include_str!("vendor/markdown.js"))
        .replace(
            "/* MARKDOWN_VENDOR_STYLES */",
            include_str!("vendor/markdown.css"),
        )
        .replace("// EDITOR_VENDOR", include_str!("vendor/editor.js"))
        .replace(
            "/* EDITOR_VENDOR_STYLES */",
            include_str!("vendor/editor.css"),
        )
}

#[cfg(feature = "desktop")]
#[path = "gui_native.rs"]
mod native;

#[cfg(feature = "desktop")]
pub use native::launch;

#[cfg(not(feature = "desktop"))]
pub fn launch(_: crate::appserver::Shared) -> anyhow::Result<()> {
    anyhow::bail!("Desktop support is disabled in this headless agent. Rebuild with --features desktop to open a native window.")
}
