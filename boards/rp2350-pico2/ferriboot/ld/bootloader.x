/* Complete cortex-m-rt linker script, replacing link.x for the bootloader
 * only. build.rs prepends the MEMORY regions (FLASH, RAM, BOOT_INFO) from
 * ferriboot-layout-rp2350-pico2. RP2350 ROM loads the image into SRAM before Reset runs.
 * PICOBIN layout: RP2350 datasheet sections 5.9.3.2/3 and 5.10.3.
 */
EXTERN(__RESET_VECTOR);
EXTERN(Reset);
ENTRY(Reset);
EXTERN(__EXCEPTIONS);
EXTERN(__INTERRUPTS);
EXTERN(DefaultHandler);
EXTERN(HardFaultTrampoline);
PROVIDE(NonMaskableInt = DefaultHandler);
PROVIDE(MemoryManagement = DefaultHandler);
PROVIDE(BusFault = DefaultHandler);
PROVIDE(UsageFault = DefaultHandler);
PROVIDE(SecureFault = DefaultHandler);
PROVIDE(SVCall = DefaultHandler);
PROVIDE(DebugMonitor = DefaultHandler);
PROVIDE(PendSV = DefaultHandler);
PROVIDE(SysTick = DefaultHandler);
PROVIDE(DefaultHandler = DefaultHandler_);
PROVIDE(HardFault = HardFault_);
PROVIDE(__pre_init = DefaultPreInit);

_ram_start = ORIGIN(RAM);
_ram_end = ORIGIN(RAM) + LENGTH(RAM);
_stack_start = _ram_end;

SECTIONS
{
    /* The only flash-resident section is non-executable ROM metadata.
     * Pad to vector alignment so all SRAM sections have the same LMA delta.
     * Embassy's automatic ImageDef is disabled; reject duplicates below.
     */
    .start_block ORIGIN(FLASH) :
    {
        __start_block_addr = .;
        LONG(0xffffded3);       /* PICOBIN_BLOCK_MARKER_START */
        LONG(0x10210142);       /* Arm Secure RP2350 executable */
        LONG(0x00000203);       /* VECTOR_TABLE, two words */
        LONG(ADDR(.vector_table));
        /* picotool seal on a BIN cannot dereference a VTOR in SRAM.
         * Supply EP/SP explicitly so it never treats SRAM as a file offset.
         */
        LONG(0x00000344);       /* ENTRY_POINT, three words */
        LONG(Reset);            /* Thumb reset handler in SRAM */
        LONG(_stack_start & 0xfffffff8);
        __load_map = .;
        LONG(0x01000406);       /* relative LOAD_MAP, one entry, four words */
        LONG(__flash_image_start - __load_map);
        LONG(ORIGIN(RAM));
        LONG(__ram_image_end - ORIGIN(RAM));
        LONG(0x00000aff);       /* LAST: ten item words */
        LONG(0);               /* self-contained block loop */
        LONG(0xab123579);       /* PICOBIN_BLOCK_MARKER_END */
        . = ALIGN(512);
        __flash_image_start = .;
    } > FLASH

    .vector_table ORIGIN(RAM) : AT(__flash_image_start)
    {
        __vector_table = .;
        LONG(_stack_start & 0xfffffff8);
        KEEP(*(.vector_table.reset_vector));
        __exceptions = .;
        KEEP(*(.vector_table.exceptions));
        __eexceptions = .;
        KEEP(*(.vector_table.interrupts));
    } > RAM

    _stext = ALIGN(ADDR(.vector_table) + SIZEOF(.vector_table), ALIGNOF(.text));
    .text _stext : AT(ABSOLUTE(__flash_image_start) + ADDR(.text) - ORIGIN(RAM))
    {
        __stext = .;
        *(.Reset);
        *(.text .text.*);
        *(.HardFaultTrampoline);
        *(.HardFault.*);
        . = ALIGN(4);
        __etext = .;
    } > RAM

    .rodata : AT(ABSOLUTE(__flash_image_start) + ADDR(.rodata) - ORIGIN(RAM))
    {
        . = ALIGN(4);
        __srodata = .;
        KEEP(*(.rodata.rp2350_enrollment));
        *(.rodata .rodata.*);
        KEEP(*(.boot_info));
        . = ALIGN(4);
        __erodata = .;
    } > RAM

    .data : AT(ABSOLUTE(__flash_image_start) + ADDR(.data) - ORIGIN(RAM))
    {
        . = ALIGN(4);
        __sdata = .;
        *(.data .data.*);
        . = ALIGN(4);
        __edata = .;
        __ram_image_end = .;
    } > RAM
    /* ROM already initialized .data at its VMA. cortex-m-rt's startup copy
     * reads/writes the same words, avoiding any post-verification flash read.
     */
    __sidata = __sdata;
    __flash_image_end = ABSOLUTE(__flash_image_start) + __ram_image_end - ORIGIN(RAM);

    .bss (NOLOAD) : ALIGN(4)
    {
        __sbss = .;
        *(.bss .bss.*);
        *(COMMON);
        . = ALIGN(4);
        __ebss = .;
    } > RAM

    .uninit (NOLOAD) : ALIGN(4)
    {
        __suninit = .;
        *(.uninit .uninit.*);
        . = ALIGN(4);
        __euninit = .;
    } > RAM
    __sheap = __euninit;
    _stack_end = __euninit;

    .ferriboot_boot_info (NOLOAD) :
    {
        _ferriboot_boot_info_start = .;
        . += LENGTH(BOOT_INFO);
    } > BOOT_INFO

    .unexpected_image_def : { KEEP(*(.start_block)); } > RAM
    .gnu.sgstubs : { KEEP(*(.gnu.sgstubs*)); } > RAM
    .got (NOLOAD) : { KEEP(*(.got .got.*)); } > RAM
    /DISCARD/ : { *(.ARM.exidx .ARM.exidx.* .ARM.extab .ARM.extab.*); }
}

/* Embassy's rt feature supplies the default peripheral interrupt aliases. */
INCLUDE device.x

ASSERT(SIZEOF(.unexpected_image_def) == 0, "enable embassy-rp imagedef-none for RAM bootloader");
ASSERT(SIZEOF(.gnu.sgstubs) == 0, "bootloader does not export Non-Secure veneers");
ASSERT(SIZEOF(.got) == 0, "dynamic relocations are unsupported");
ASSERT(__exceptions == ADDR(.vector_table) + 8, "missing reset vector");
ASSERT(__eexceptions == ADDR(.vector_table) + 64, "missing exception vectors");
ASSERT(SIZEOF(.vector_table) == (16 + 53) * 4, "incorrect RP2350 interrupt vectors");
ASSERT(ORIGIN(RAM) % 512 == 0, "SRAM VTOR must be 512-byte aligned");
ASSERT(SIZEOF(.start_block) == 512, "incorrect ROM image-definition placement");
ASSERT(__flash_image_end <= ORIGIN(FLASH) + LENGTH(FLASH), "RAM image exceeds bootloader flash partition");
ASSERT(__sdata % 4 == 0 && __edata % 4 == 0, "unaligned startup data");
ASSERT(__sbss % 4 == 0 && __ebss % 4 == 0, "unaligned startup BSS");
ASSERT(_stack_start % 8 == 0 && _stack_start > _stack_end, "no aligned bootloader stack space");
