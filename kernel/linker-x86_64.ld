/* Oceans kernel image layout for x86_64 (higher half, Limine protocol). */
OUTPUT_FORMAT(elf64-x86-64)
ENTRY(kernel_entry)

PHDRS
{
    limine_requests PT_LOAD FLAGS(6); /* RW: the bootloader writes responses */
    text            PT_LOAD FLAGS(5); /* RX */
    rodata          PT_LOAD FLAGS(4); /* R  */
    data            PT_LOAD FLAGS(6); /* RW */
}

SECTIONS
{
    /* Top 2 GiB of the address space, required by -C code-model=kernel. */
    . = 0xffffffff80000000;

    .limine_requests : {
        KEEP(*(.limine_requests_start))
        KEEP(*(.limine_requests))
        KEEP(*(.limine_requests_end))
    } :limine_requests

    . = ALIGN(CONSTANT(MAXPAGESIZE));
    .text : {
        *(.text .text.*)
    } :text

    . = ALIGN(CONSTANT(MAXPAGESIZE));
    .rodata : {
        *(.rodata .rodata.*)
    } :rodata

    /* Fully resolved at link time (static relocation model); read-only. */
    .got : {
        *(.got .got.*)
    } :rodata

    . = ALIGN(CONSTANT(MAXPAGESIZE));
    .data : {
        *(.data .data.*)
    } :data

    .bss : {
        *(.bss .bss.*)
        *(COMMON)
    } :data

    /DISCARD/ : {
        *(.eh_frame*)
        *(.note .note.*)
    }
}
