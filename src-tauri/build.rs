fn main() {
    // tauri_build::build() embeds the Windows exe manifest (Common-Controls v6 SxS
    // dependency, DPI awareness, icon). Without it, cross-compiled binaries fail on
    // Windows with "TaskDialogIndirect could not be located in comctl32" because lld
    // does not auto-insert the manifest that msvc link.exe would.
    tauri_build::build();
}
