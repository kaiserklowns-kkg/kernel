/* Oceans kernel image layout for x86_64 (higher half, Limine protocol).
 *
 * Each segment starts on a page boundary and the __*_start / __*_end symbols
 * mark them, so the kernel can map every segment with exactly the
 * permissions below (ADR-0009, W^X). */
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
    __kernel_start = .;

    __requests_start = .;
    .limine_requests : {
        KEEP(*(.limine_requests_start))
        KEEP(*(.limine_requests))
        KEEP(*(.limine_requests_end))
    } :limine_requests
    . = ALIGN(CONSTANT(MAXPAGESIZE));
    __requests_end = .;

    __text_start = .;
    .text : {
        *(.text .text.*)
    } :text
    . = ALIGN(CONSTANT(MAXPAGESIZE));
    __text_end = .;

    __rodata_start = .;
    .rodata : {
        *(.rodata .rodata.*)
    } :rodata

    /* Fully resolved at link time (static relocation model); read-only. */
    .got : {
        *(.got .got.*)
    } :rodata
    . = ALIGN(CONSTANT(MAXPAGESIZE));
    __rodata_end = .;

    __data_start = .;
    .data : {
        *(.data .data.*)
    } :data

    .bss : {
        *(.bss .bss.*)
        *(COMMON)
    } :data
    . = ALIGN(CONSTANT(MAXPAGESIZE));
    __data_end = .;
    __kernel_end = .;

    /DISCARD/ : {
        *(.eh_frame*)
        *(.note .note.*)
    }
}
