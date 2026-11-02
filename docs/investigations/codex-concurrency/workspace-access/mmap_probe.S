// Freestanding Linux/aarch64 adversary. No host execution or libc dependency.
.text
.global _start
_start:
    ldr x9, [sp]
    cmp x9, #2
    b.lt .Ldefault
    ldr x1, [sp, #16]
    b .Lopen
.Ldefault:
    adr x1, .Lpath
.Lopen:
    mov x0, #-100
    mov x2, #66
    mov x3, #384
    mov x8, #56
    svc #0
    cmp x0, #0
    b.lt .Lfail
    mov x19, x0
    mov x1, #4096
    mov x8, #46
    svc #0
    cmp x0, #0
    b.lt .Lfail
    mov x0, #0
    mov x1, #4096
    mov x2, #3
    mov x3, #1
    mov x4, x19
    mov x5, #0
    mov x8, #222
    svc #0
    cmp x0, #0
    b.lt .Lfail
    mov x20, x0
    mov x21, #0
.Lloop:
    add x21, x21, #1
    str x21, [x20]
    mov x0, x20
    mov x1, #4096
    mov x2, #4
    mov x8, #227
    svc #0
    cmp x0, #0
    b.lt .Lflush_error
    adr x1, .Lprogress
    mov x2, #12
    b .Lprint
.Lflush_error:
    adr x1, .Lflush
    mov x2, #17
.Lprint:
    mov x0, #1
    mov x8, #64
    svc #0
    adr x0, .Lts
    mov x1, #0
    mov x8, #101
    svc #0
    cmp x21, #600
    b.lt .Lloop
    mov x0, #0
    b .Lexit
.Lfail:
    mov x0, #1
    adr x1, .Lerror
    mov x2, #11
    mov x8, #64
    svc #0
    mov x0, #1
.Lexit:
    mov x8, #93
    svc #0
.Lpath: .asciz "/project/mmap.bin"
.Lprogress: .ascii "MMAP_EFFECT\n"
.Lflush: .ascii "MMAP_FLUSH_ERROR\n"
.Lerror: .ascii "MMAP_ERROR\n"
.balign 8
.Lts: .quad 0, 20000000
