fn main() {
    // Register our app-defined IPC commands with the ACL so the (remote-origin)
    // popover webview is allowed to call them. `set_popover_height` lets the
    // frontend snap the window to its measured content height (auto-fit, no dead
    // whitespace). Autogenerates the `allow-set-popover-height` permission, which
    // the `default` capability grants to the popover window.
    tauri_build::try_build(
        tauri_build::Attributes::new().app_manifest(
            tauri_build::AppManifest::new().commands(&["set_popover_height"]),
        ),
    )
    .expect("failed to run tauri-build");
}
