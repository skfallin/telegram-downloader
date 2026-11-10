// Su Windows mette l'icona dentro l'eseguibile.
fn main() {
    #[cfg(windows)]
    if let Err(e) = winresource::WindowsResource::new().set_icon("assets/icon.ico").compile() {
        println!("cargo:warning=icona non inserita: {e}");
    }
}
