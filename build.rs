fn main() {
    // Încorporează icon.ico în executabilul de Windows
    if std::path::Path::new("icon.ico").exists() {
        let mut res = winres::WindowsResource::new();
        res.set_icon("icon.ico");
        res.compile().unwrap();
    }

    let config = slint_build::CompilerConfiguration::new()
        .with_style(String::from("fluent"));
        
    slint_build::compile_with_config("ui/main.slint", config)
        .expect("Eroare la compilarea fisierului Slint");
}