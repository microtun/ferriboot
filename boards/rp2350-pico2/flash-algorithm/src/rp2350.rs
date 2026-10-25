//! RP2350 hardware access: Secure Arm core0, Pico 2 (12 MHz crystal, W25Q32 NOR).
//! Flash erase/program go through the bootrom; everything runs from SRAM.
use crate::Error;
use crate::protocol::{FLASH_SIZE, SECTOR, Sector};
use core::arch::asm;
use core::ptr::{read_volatile, write_volatile};
use cortex_m::asm::{dsb, isb};
use rp_pac as pac;

const XIP_BASE: usize = 0x1000_0000;
const POLL_LIMIT: u32 = 1_000_000;
/// QMI clock divider for fast reads: 150 MHz / 2 = 75 MHz.
const FAST_CLKDIV: u8 = 2;

type RomFn = unsafe extern "C" fn();
type RomEraseFn = unsafe extern "C" fn(u32, usize, u32, u8);
type RomProgramFn = unsafe extern "C" fn(u32, *const u8, usize);
type RomReadModeFn = unsafe extern "C" fn(u32, u8);

/// Bootrom flash functions, looked up once in `init`.
struct Rom {
    connect_internal_flash: RomFn,
    flash_exit_xip: RomFn,
    flash_flush_cache: RomFn,
    flash_enter_cmd_xip: RomFn,
    flash_reset_address_trans: RomFn,
    flash_range_erase: RomEraseFn,
    flash_range_program: RomProgramFn,
    flash_select_xip_read_mode: RomReadModeFn,
}

pub struct Rp2350 {
    rom: Option<Rom>,
    read_mode: u32,
}

impl Rp2350 {
    pub const fn new() -> Self {
        Self {
            rom: None,
            read_mode: 0,
        }
    }

    fn rom(&self) -> &Rom {
        self.rom.as_ref().unwrap() // Only reachable after a successful `init`.
    }

    fn flush_cache(&self) {
        // SAFETY: ROM routine resolved in `init`; runs from SRAM with core1
        // parked, so nothing executes from XIP while the cache is flushed.
        unsafe { (self.rom().flash_flush_cache)() }
    }

    /// Slow (03h, /12) XIP reads, used as a reference for the fast mode.
    fn slow_xip(&self) {
        // SAFETY: ROM routine resolved in `init`; nothing executes from XIP.
        unsafe { (self.rom().flash_enter_cmd_xip)() }
        self.flush_cache();
    }

    /// 75 MHz dual/quad XIP. Keeps the ROM's chip-select timing, but samples
    /// one system cycle late and allows a longer cooldown between reads.
    fn fast_xip(&self) {
        // SAFETY: ROM routine resolved in `init`; `read_mode` was chosen by
        // `select_read_mode` for the detected flash part.
        unsafe { (self.rom().flash_select_xip_read_mode)(self.read_mode, FAST_CLKDIV) }
        for window in 0..2 {
            pac::QMI.mem(window).timing().modify(|w| {
                w.set_rxdelay(2);
                w.set_cooldown(3);
            });
        }
        pac::QMI.direct_csr().modify(|w| w.set_rxdelay(2));
        barrier();
    }

    fn connect_flash(&mut self, scratch: &mut Sector) -> Result<(), Error> {
        // SAFETY: the RP2350 bootrom is mapped at address 0 on every chip
        // revision this fixture supports; `lookup` checks the table pointer.
        let rom = unsafe { Rom::lookup()? };
        // SAFETY: ROM routines; this code runs from SRAM with core1 parked.
        unsafe { (rom.connect_internal_flash)() };
        // SAFETY: as above.
        unsafe { (rom.flash_exit_xip)() };
        init_clocks()?;

        // Secure XIP cache on and address translation reset, so reads hit physical NOR.
        pac::XIP_CTRL.ctrl().modify(|w| {
            w.set_power_down(false);
            w.set_en_secure(true);
        });
        // SAFETY: ROM routine; nothing executes from XIP.
        unsafe { (rom.flash_reset_address_trans)() }
        self.rom = Some(rom);
        self.flush_cache();

        let jedec_id = flash_command(0x9f, 3)?;
        let status2 = flash_command(0x35, 1)?;
        self.read_mode = select_read_mode(jedec_id, status2)?;

        // Check the fast mode against slow reads of the first and last sectors.
        for offset in [0, FLASH_SIZE - SECTOR] {
            self.slow_xip();
            for (i, word) in scratch.iter_mut().enumerate() {
                *word = self.read_word(offset + i * 4);
            }
            self.fast_xip();
            self.flush_cache();
            if scratch
                .iter()
                .enumerate()
                .any(|(i, &word)| self.read_word(offset + i * 4) != word)
            {
                self.slow_xip();
                return Err(Error::Verify);
            }
        }
        Ok(())
    }

    /// Bring up clocks and the flash interface and park core1. Clobbers `scratch`.
    pub fn init(&mut self, scratch: &mut Sector) -> Result<(), Error> {
        cortex_m::interrupt::disable();
        pac::WATCHDOG
            .ctrl()
            .write_value(pac::watchdog::regs::Ctrl(0));
        enable_rcp()?;
        park_core1()?;
        if let Err(error) = self.connect_flash(scratch) {
            self.release_core1();
            return Err(error);
        }
        // probe-rs returns to a BKPT at 0x20000000. Put `b .` right after it so
        // a core resumed by a detaching debugger spins instead of running off.
        // SAFETY: probe-rs reserves 0x20000000..0x20000004 for its return
        // trap (see the linker script); this halfword is ours to patch.
        unsafe { write_volatile(0x2000_0002 as *mut u16, 0xe7fe) };
        Ok(())
    }

    pub fn release_core1(&self) {
        pac::PSM.frce_off().modify(|w| w.set_proc1(false));
        barrier();
    }

    /// Read one word of physical flash. `offset` is word-aligned.
    pub fn read_word(&self, offset: usize) -> u32 {
        // SAFETY: callers pass word-aligned offsets below FLASH_SIZE, and the
        // XIP window maps all of it once `init` succeeded.
        unsafe { read_volatile((XIP_BASE + offset) as *const u32) }
    }

    /// Bracket the erase/program calls for one sector.
    pub fn begin_writes(&self) {
        self.fast_xip();
    }

    pub fn end_writes(&self) {
        self.flush_cache();
        self.fast_xip();
    }

    pub fn erase_sector(&self, offset: usize) {
        const SECTOR_ERASE_20H: u8 = 0x20;
        // SAFETY: ROM routine; callers bracket it with begin/end_writes, run
        // from SRAM with core1 parked, and pass a sector-aligned offset.
        unsafe {
            (self.rom().flash_range_erase)(offset as u32, SECTOR, SECTOR as u32, SECTOR_ERASE_20H)
        }
    }

    /// Program whole, page-aligned pages.
    pub fn program(&self, offset: usize, data: &[u8]) {
        // SAFETY: ROM routine reading `data` (a live slice) synchronously;
        // same execution context as `erase_sector`.
        unsafe { (self.rom().flash_range_program)(offset as u32, data.as_ptr(), data.len()) }
    }
}

type RomLookupFn = unsafe extern "C" fn(code: u32, mask: u32) -> usize;

/// Resolve one bootrom function by its two-letter tag.
///
/// # Safety
/// `lookup` is the ROM's table-lookup function and `F` is the function-pointer
/// type matching the ABI of the ROM function named by `tag`.
unsafe fn resolve<F: Copy>(lookup: RomLookupFn, tag: &[u8; 2]) -> Result<F, Error> {
    const { assert!(core::mem::size_of::<F>() == core::mem::size_of::<usize>()) };
    // SAFETY: the ROM's documented table lookup (code, mask) ABI.
    let address = unsafe { lookup(u16::from_le_bytes(*tag).into(), 0x4) };
    if address == 0 {
        return Err(Error::RomMissing);
    }
    // SAFETY: `address` is the ROM function named by `tag`, whose ABI is `F`
    // (caller contract), and `F` is pointer-sized (asserted above).
    Ok(unsafe { core::mem::transmute_copy::<usize, F>(&address) })
}

impl Rom {
    /// Resolve the Secure Arm bootrom functions.
    ///
    /// # Safety
    /// The RP2350 bootrom must be mapped at address 0.
    unsafe fn lookup() -> Result<Self, Error> {
        // ROM v1 (A1) has a 32-bit lookup pointer at 0x18; later ROMs a 16-bit one at 0x16.
        // SAFETY: the bootrom header is mapped (caller contract).
        let version = unsafe { read_volatile(0x13 as *const u8) };
        let lookup = if version == 1 {
            // SAFETY: as above; 0x18 is word aligned.
            unsafe { read_volatile(0x18 as *const u32) as usize }
        } else {
            // SAFETY: as above; 0x16 is halfword aligned.
            unsafe { read_volatile(0x16 as *const u16) as usize }
        };
        if lookup & 1 == 0 {
            return Err(Error::RomAbi); // Not a Thumb function pointer.
        }
        // SAFETY: an odd (Thumb) address read from the ROM header is the
        // ROM's table-lookup function, with this signature.
        let lookup: RomLookupFn = unsafe { core::mem::transmute(lookup) };
        // SAFETY (each `resolve` below): the tag names the ROM function whose
        // ABI is the field's type, per the RP2350 datasheet §5.4.
        Ok(Self {
            // SAFETY: "IF" is connect_internal_flash(void).
            connect_internal_flash: unsafe { resolve(lookup, b"IF") }?,
            // SAFETY: "EX" is flash_exit_xip(void).
            flash_exit_xip: unsafe { resolve(lookup, b"EX") }?,
            // SAFETY: "FC" is flash_flush_cache(void).
            flash_flush_cache: unsafe { resolve(lookup, b"FC") }?,
            // SAFETY: "CX" is flash_enter_cmd_xip(void).
            flash_enter_cmd_xip: unsafe { resolve(lookup, b"CX") }?,
            // SAFETY: "RE" is flash_range_erase(addr, count, block_size, block_cmd).
            flash_range_erase: unsafe { resolve(lookup, b"RE") }?,
            // SAFETY: "RP" is flash_range_program(addr, data, count).
            flash_range_program: unsafe { resolve(lookup, b"RP") }?,
            // SAFETY: "XM" is flash_select_xip_read_mode(mode, clkdiv).
            flash_select_xip_read_mode: unsafe { resolve(lookup, b"XM") }?,
            // SAFETY: "RA" is flash_reset_address_trans(void).
            flash_reset_address_trans: unsafe { resolve(lookup, b"RA") }?,
        })
    }
}

fn wait_for_clock(done: impl FnMut() -> bool) -> Result<(), Error> {
    wait_until(POLL_LIMIT, Error::ClockTimeout, done)
}

fn barrier() {
    dsb();
    isb();
}

fn wait_until(limit: u32, error: Error, mut done: impl FnMut() -> bool) -> Result<(), Error> {
    if (0..limit).any(|_| done()) {
        Ok(())
    } else {
        Err(error)
    }
}

/// Bootrom helpers start with RCP (coprocessor 7) canary instructions. A
/// debugger that halted the core before the bootrom ran leaves the RCP unsalted,
/// so enable CP7 and, only if needed, salt it with a fixed fixture-only value
/// for both cores (as OpenOCD does).
fn enable_rcp() -> Result<(), Error> {
    const VALID: u32 = 0xa500_a500;
    const UNSALTED: u32 = 0x00c3_00c3;
    const SALT: u32 = 0xe000_ed88;
    // rp-pac has no PPB block; CPACR is the Armv8-M SCB register, and CP7
    // is bits 15:14. Steal only the SCB: nothing else owns it in this image.
    // SAFETY: the SCB register block is always mapped on Cortex-M33.
    let scb = unsafe { &*cortex_m::peripheral::SCB::PTR };
    // SAFETY: CPACR read-modify-write; enabling CP7 cannot affect memory.
    unsafe { scb.cpacr.modify(|cpacr| cpacr | (0b11 << 14)) };
    barrier();
    let status = || {
        let status: u32;
        // SAFETY: reads the RCP status register; no memory access.
        unsafe {
            asm!("mrc p7, #1, {}, c0, c0, #0", out(reg) status, options(nostack, preserves_flags))
        };
        status
    };
    match status() {
        VALID => return Ok(()),
        UNSALTED => {}
        _ => return Err(Error::RcpNotReady),
    }
    // SAFETY: salts the RCP of both cores (as OpenOCD does); no memory access.
    unsafe {
        asm!(
            "mcrr p7, #8, {salt}, {salt}, c0",
            "mcrr p7, #8, {salt}, {salt}, c1",
            "sev",
            salt = in(reg) SALT,
            options(nostack, preserves_flags),
        );
    }
    if status() == VALID {
        Ok(())
    } else {
        Err(Error::RcpNotReady)
    }
}

/// Force core1 off for the session so nothing else runs from flash.
fn park_core1() -> Result<(), Error> {
    pac::PSM.frce_off().modify(|w| w.set_proc1(true));
    wait_until(100_000, Error::Hardware, || !pac::PSM.done().read().proc1())
}

/// 12 MHz XOSC -> PLL_SYS (VCO 1500 MHz, /5 /2) -> 150 MHz clk_sys.
fn init_clocks() -> Result<(), Error> {
    use pac::{clocks::vals as clk, pll::regs as pll_regs, xosc::vals as xosc};
    let (clocks, pll) = (pac::CLOCKS, pac::PLL_SYS);

    // Start the crystal (harmless if it is already running).
    pac::XOSC.startup().write(|w| w.set_delay(47)); // ~1 ms
    pac::XOSC.ctrl().write(|w| {
        w.set_freq_range(xosc::CtrlFreqRange::_1_15MHZ);
        w.set_enable(xosc::Enable::ENABLE);
    });
    wait_for_clock(|| pac::XOSC.status().read().stable())?;

    // Run clk_ref and clk_sys from the crystal while PLL_SYS is reset.
    clocks
        .clk_sys_resus_ctrl()
        .write_value(pac::clocks::regs::ClkSysResusCtrl(0));
    clocks
        .clk_ref_ctrl()
        .modify(|w| w.set_src(clk::ClkRefCtrlSrc::XOSC_CLKSRC));
    wait_for_clock(|| clocks.clk_ref_selected().read().0 & 0xf == 1 << 2)?;
    clocks.clk_ref_div().write(|w| w.set_int(1));
    clocks
        .clk_sys_ctrl()
        .modify(|w| w.set_src(clk::ClkSysCtrlSrc::CLK_REF));
    wait_for_clock(|| clocks.clk_sys_selected().read().0 & 0b11 == 0b01)?;
    clocks.clk_sys_div().write(|w| w.set_int(1));

    pac::RESETS.reset().modify(|w| w.set_pll_sys(true));
    pac::RESETS.reset().modify(|w| w.set_pll_sys(false));
    wait_for_clock(|| pac::RESETS.reset_done().read().pll_sys())?;

    pll.cs().write(|w| w.set_refdiv(1));
    pll.fbdiv_int().write(|w| w.set_fbdiv_int(125));
    // rp-pac's `write` starts from 0, not the reset value 0x2d: keep DSMPD
    // and POSTDIVPD set explicitly, powering up only the PLL and its VCO.
    let mut pwr = pll_regs::Pwr(0);
    pwr.set_dsmpd(true);
    pwr.set_postdivpd(true);
    pll.pwr().write_value(pwr);
    wait_for_clock(|| pll.cs().read().lock())?;
    pll.prim().write(|w| {
        w.set_postdiv1(5);
        w.set_postdiv2(2);
    });
    pll.pwr().modify(|w| w.set_postdivpd(false));

    clocks
        .clk_sys_ctrl()
        .modify(|w| w.set_auxsrc(clk::ClkSysCtrlAuxsrc::CLKSRC_PLL_SYS));
    clocks
        .clk_sys_ctrl()
        .modify(|w| w.set_src(clk::ClkSysCtrlSrc::CLKSRC_CLK_SYS_AUX));
    wait_for_clock(|| clocks.clk_sys_selected().read().0 & 0b11 == 0b10)?;
    barrier();
    Ok(())
}

/// Run a read-only flash command in QMI direct mode and return the `count`
/// bytes that follow it, big-endian. Chip select is always released.
fn flash_command(command: u8, count: u32) -> Result<u32, Error> {
    let qmi = pac::QMI;
    let csr = qmi.direct_csr().read();
    let (clkdiv, rxdelay) = (csr.clkdiv(), csr.rxdelay());
    // Every field that is non-zero at reset (CLKDIV) is set explicitly, so
    // starting from 0 matches the previous reset-value-based write.
    let direct = |enable: bool, select: bool| {
        qmi.direct_csr().write(|w| {
            w.set_clkdiv(clkdiv);
            w.set_rxdelay(rxdelay);
            w.set_en(enable);
            w.set_assert_cs0n(select);
        })
    };
    let idle = || !qmi.direct_csr().read().busy();

    direct(true, false);
    let result = (|| {
        wait_until(POLL_LIMIT, Error::Hardware, idle)?;
        wait_until(POLL_LIMIT, Error::Hardware, || {
            let empty = qmi.direct_csr().read().rxempty();
            if !empty {
                let _ = qmi.direct_rx().read(); // Discard stale bytes.
            }
            empty
        })?;
        direct(true, true);
        let mut value = 0;
        for n in 0..=count {
            let byte = if n == 0 { command } else { 0 };
            qmi.direct_tx().write(|w| w.set_data(byte.into()));
            wait_until(POLL_LIMIT, Error::Hardware, || {
                !qmi.direct_csr().read().rxempty()
            })?;
            let received = qmi.direct_rx().read().0 & 0xff;
            if n != 0 {
                value = value << 8 | received;
            }
        }
        wait_until(POLL_LIMIT, Error::Hardware, idle)?;
        Ok(value)
    })();
    direct(false, false);
    result
}

/// Supported flash parts and how to read them fast, as a mode for the bootrom's
/// `flash_select_xip_read_mode`. Status register 2 is never written, so quad
/// I/O is used only if the QE bit is already set.
fn select_read_mode(jedec_id: u32, status2: u32) -> Result<u32, Error> {
    const W25Q32: [u32; 2] = [0xef_7016, 0xef_4016]; // W25Q32RV (Pico 2), W25Q32JV
    const QUAD_ENABLE: u32 = 1 << 1;
    const DUAL_IO_BBH: u32 = 2;
    const QUAD_IO_EBH: u32 = 3;
    if !W25Q32.contains(&jedec_id) {
        Err(Error::FlashUnsupported)
    } else if status2 & QUAD_ENABLE != 0 {
        Ok(QUAD_IO_EBH)
    } else {
        Ok(DUAL_IO_BBH)
    }
}
