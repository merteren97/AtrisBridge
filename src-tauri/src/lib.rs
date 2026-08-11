mod commands;
mod models;
mod scanner;
mod storage;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            commands::list_workspaces,
            commands::add_workspace,
            commands::remove_workspace,
            commands::scan_workspace,
            commands::initialize_ignore_file,
        ])
        .run(tauri::generate_context!())
        .expect("error while running AtrisBridge");
}
