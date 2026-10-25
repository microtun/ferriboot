use ferriboot_build::configure_rp2350_application;
use ferriboot_layout_rp2350_pico2 as layout;

fn main() {
    configure_rp2350_application(layout::CHIP, layout::VALUES, layout::MAX_IMAGE_SIZE);
}
