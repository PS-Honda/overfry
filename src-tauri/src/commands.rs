/// Temporary greeting command — replaced in Milestone 2 with real CRUD commands.
#[tauri::command]
pub fn greet(name: &str) -> String {
    format!("Overfry says hello, {}!", name)
}
