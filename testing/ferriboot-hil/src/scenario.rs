//! Helpers shared by physical smoke checks and QEMU scenarios (`tests/common/mod.rs`
//! re-exports them). They depend only on the `@fbt` report format of the
//! example firmware and on a composed [`FlashImage`].

use std::time::Duration;

use ferriboot_core::Slot;

use crate::{
    dut::Dut,
    flash::FlashImage,
    hooks::{Behaviour, HookConfig},
};

/// Boot to the application, including validation and a full swap.
pub const BOOT: Duration = Duration::from_secs(60);
/// How long to watch for something that must not happen.
pub const QUIET: Duration = if cfg!(feature = "qemu") {
    Duration::from_millis(100)
} else {
    Duration::from_secs(8)
};

/// Set how image versions behave; unlisted versions confirm and idle.
pub fn behave(flash: &FlashImage, behaviours: &[(&str, Behaviour)]) {
    let config = behaviours
        .iter()
        .fold(HookConfig::default(), |c, (v, b)| c.with(v, *b));
    flash.configure(&config);
}

/// Value of `key=value` in a space-separated `@fbt` report line.
pub fn field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    line.split(' ')
        .find_map(|kv| kv.strip_prefix(key)?.strip_prefix('='))
}

/// Expect the next application start (`@fbt hello version=…`) after `from` to
/// be image `version`.
pub fn expect_running(dut: &Dut, from: usize, version: &str) -> usize {
    let (i, hello) = dut.expect_from(from, "@fbt hello", BOOT).unwrap();
    let expected = format!("{version}+0");
    assert_eq!(
        field(&hello, "version"),
        Some(expected.as_str()),
        "wrong image"
    );
    i
}

/// Version of the authenticated image in `slot`, as text.
pub fn version(flash: &FlashImage, slot: Slot) -> Option<String> {
    flash.version(slot).map(|v| v.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_are_matched_by_whole_key() {
        let line = "@fbt bootinfo mode=SwapScratch slot=Primary max=1929216";
        assert_eq!(field(line, "mode"), Some("SwapScratch"));
        assert_eq!(field(line, "max"), Some("1929216"));
        assert_eq!(field(line, "slo"), None);
        assert_eq!(field(line, "missing"), None);
    }
}
