// Windowsのrelease buildでコンソールウィンドウを出さないためのTauriテンプレート規約。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    gento_tauri_lib::run()
}
