//! The third stage links like an ESP application; ferriboot-esp's
//! `boot-runtime` feature builds and embeds the RAM handoff loader.
fn main() {
    ferriboot_build::configure_esp_application(esp32c3_layout::CHIP, esp32c3_layout::CHIP_JSON);
    ferriboot_build::emit_public_keys("../../../keys/dev-p256.pub.pem");
}
