/* Ferriboot RP2350 application linker contract, exported by ferriboot-rp.
 * Included by the memory.x that ferriboot_build::configure_rp2350_application
 * writes into the application's build directory, after its MEMORY regions
 * (FLASH, RAM, BOOT_INFO). Chip rules only: the board decides the regions.
 */

/* Collect ROM image definitions and binary info so they can be rejected
 * below: Ferriboot, not the bootrom, must start the application. */
SECTIONS
{
    .start_block : ALIGN(4)
    {
        __start_block_addr = .;

        KEEP(*(.start_block));
        KEEP(*(.boot_info));
    } > FLASH
} INSERT AFTER .vector_table;

_stext = ALIGN(ADDR(.start_block) + SIZEOF(.start_block), 8);

/* Inserting a separate NOLOAD region must not move the normal heap/stack
 * floor to the end of the reserved handoff region. */
_stack_end = __euninit;
__sheap     = __euninit;

SECTIONS
{
    .ferriboot_boot_info (NOLOAD) :
    {
        _ferriboot_boot_info_start = .;
        . += LENGTH(BOOT_INFO);
    } > BOOT_INFO
} INSERT AFTER .uninit;

ASSERT(
    ADDR(.vector_table) == ORIGIN(FLASH),
    "vector table must start at payload origin"
);

ASSERT(
    ORIGIN(FLASH) % 512 == 0,
    "RP2350 VTOR must be 512-byte aligned"
);

ASSERT(
    SIZEOF(.start_block) == 0,
    "incorrect ROM image-definition policy"
);
