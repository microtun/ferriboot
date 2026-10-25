use ferriboot_layout_qemu as layout;

fn main() {
    ferriboot_build::check_platform_chip("ferriboot-qemu", layout::CHIP);
    ferriboot_build::emit_public_keys("../../../keys/dev-p256.pub.pem");
    let memory = format!(
        "MEMORY {{ FLASH : ORIGIN = {:#x}, LENGTH = {:#x}\n RAM : ORIGIN = {:#x}, LENGTH = {:#x} }}\n",
        0,
        layout::BOOTLOADER_SIZE,
        layout::RAM_BASE,
        layout::BOOT_INFO_ADDRESS - layout::RAM_BASE
    );
    ferriboot_build::write_out("memory.x", memory);
    println!(
        "cargo:rustc-link-search={}",
        ferriboot_build::out_dir().display()
    );
    println!("cargo:rerun-if-changed=build.rs");
}
