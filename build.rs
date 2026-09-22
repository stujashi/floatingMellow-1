fn main() {
    // This only runs when compiling for Windows
    if std::env::var("CARGO_CFG_TARGET_OS").unwrap() == "windows" {
        let mut res = winres::WindowsResource::new();
        // Point this to your .ico file
        res.set_icon("icon.ico");
        res.compile().unwrap();
    }
}