/* Oceans user program layout (ADR-0014). One page-aligned segment per
 * permission set: the loader rejects writable+executable segments and
 * segments sharing a page. */
OUTPUT_FORMAT(elf64-x86-64)
ENTRY(_start)

PHDRS
{
    text   PT_LOAD FLAGS(5); /* R-X */
    rodata PT_LOAD FLAGS(4); /* R-- */
    data   PT_LOAD FLAGS(6); /* RW- */
}

SECTIONS
{
    . = 0x400000;

    .text : { *(.text .text.*) } :text
    . = ALIGN(4096);

    .rodata : { *(.rodata .rodata.*) } :rodata
    .got : { *(.got .got.*) } :rodata
    . = ALIGN(4096);

    .data : { *(.data .data.*) } :data
    .bss : { *(.bss .bss.*) *(COMMON) } :data

    /* Requested capabilities (oceans_rt::manifest!), read by the shell;
     * not loaded into memory. */
    .oceans.manifest 0 (INFO) : { KEEP(*(.oceans.manifest)) }

    /DISCARD/ : { *(.eh_frame*) *(.note .note.*) *(.comment) }
}
