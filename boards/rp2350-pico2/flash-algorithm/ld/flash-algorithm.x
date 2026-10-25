/* probe-rs 0.32 inserts its 4-byte BKPT return trap at 0x20000000. */
ENTRY(Init)
SECTIONS {
    . = 0x20000004;
    .text : { *(.text*) *(.rodata*) }
    . = ALIGN(128);
    .data : {
        _data_start = .;
        *(.data*) *(.bss*) *(COMMON)
        /* Include zero-initialized Rust statics in the uploaded image. */
        BYTE(0)
        . = ALIGN(4);
    }
    _end = .;
    ASSERT(_end < 0x20004000, "algorithm exceeds SRAM code/data budget")
    /DISCARD/ : { *(.ARM.exidx*) *(.ARM.extab*) *(.comment*) *(.eh_frame*) }
}
