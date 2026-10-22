use ferriboot_build::configure_rp2350_application;
use rp2350_layout as layout;

fn main() {
    configure_rp2350_application(layout::CHIP, layout::MAX_IMAGE_SIZE);
}
