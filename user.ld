/* user.ld */
ENTRY(_start)

SECTIONS
{
    /* Start mapping at 0x400000 */
    . = 0x400000;

    .text : ALIGN(0x1000) {
        *(.text)
    }

    .rodata : ALIGN(0x1000) {
        *(.rodata)
    }

    .data : ALIGN(0x1000) {
        *(.data)
    }

    /* Place .magic at 0x403000 to be completely safe from .rodata and .eh_frame */
    . = 0x403000;
    .magic : {
        *(.magic)
    }

    .bss : ALIGN(0x1000) {
        *(.bss)
    }
}