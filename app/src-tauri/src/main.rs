// Empeche l'ouverture d'une fenetre console en plus de la fenetre Tauri sur Windows en release.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    oneclick_kvm_app_lib::run();
}
