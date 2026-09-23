ENTRY(_start)

PHDRS
{
    text PT_LOAD FLAGS(5);
    rodata PT_LOAD FLAGS(4);
    data PT_LOAD FLAGS(6);
}

SECTIONS
{
    /* P4 slot 255 is the kernel's isolated user address range. Leave the
       first 1 MiB available for the process heap. */
    . = 0x00007F8000100000;

    .text : ALIGN(0x1000) {
        *(.text .text.* .ltext .ltext.*)
    } :text

    .rodata : ALIGN(0x1000) {
        *(.rodata .rodata.* .lrodata .lrodata.*)
    } :rodata

    .data : ALIGN(0x1000) {
        *(.data .data.* .ldata .ldata.*)
    } :data

    .bss : ALIGN(0x1000) {
        *(.bss .bss.* .lbss .lbss.* COMMON)
    } :data

    /DISCARD/ : {
        *(.comment)
        *(.eh_frame)
        *(.note.*)
    }
}
