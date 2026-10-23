//! picoseal BIN-only compatibility fixtures; golden output from picotool's
//! external-sign branch. These test the actual in-tree implementation.
use ferriboot_tools_rp2350::seal;
// The low-level ROM codec is crate-private. Public bootloader sealing is tested
// in `seal.rs`, alongside its fake-but-structurally-real Ferriboot ROM header.

#[test]
fn local_key_generation_is_valid_secp256k1() {
    let (private, public) = seal::generate_pair().unwrap();
    assert!(private.starts_with("-----BEGIN EC PRIVATE KEY-----"));
    assert!(public.starts_with("-----BEGIN PUBLIC KEY-----"));
}
