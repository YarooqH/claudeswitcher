// Embeds assets/icon.ico as the executable's icon. Resource 1 is also the one GPUI
// puts on its window, so this covers Explorer, the taskbar and Alt-Tab.
fn main() {
    println!("cargo:rerun-if-changed=assets/app.rc");
    println!("cargo:rerun-if-changed=assets/icon.ico");

    // Cosmetic only: without a resource compiler the app still builds, just
    // without the icon.
    let _ = embed_resource::compile("assets/app.rc", embed_resource::NONE);
}
