// Compatibility shims for musl libc (which doesn't have *64 functions)
// Provide strong definitions of *64 functions that map to standard ones.

#define _GNU_SOURCE
#include <stdio.h>
#include <errno.h>
#include <fcntl.h>
#include <stdarg.h>
#include <sys/types.h>

// fopen64 -> fopen
FILE *fopen64(const char *pathname, const char *mode) {
    return fopen(pathname, mode);
}

// freopen64 -> freopen
FILE *freopen64(const char *pathname, const char *mode, FILE *stream) {
    return freopen(pathname, mode, stream);
}

// tmpfile64 -> tmpfile
FILE *tmpfile64(void) {
    return tmpfile();
}

// Additional *64 functions that might be needed
// (not used by Lua but for completeness)
// open64 is not needed for glibc builds; removing to avoid ENOSYS issues.
// If needed for musl builds, implement properly.
// int open64(const char *pathname, int flags, ...) {
//     mode_t mode = 0;
//     if (flags & O_CREAT) {
//         va_list ap;
//         va_start(ap, flags);
//         mode = va_arg(ap, mode_t);
//         va_end(ap);
//     }
//     return open(pathname, flags, mode);
// }

// Note: We don't need to implement all *64 functions, only those referenced.
// Lua references fopen64, freopen64, tmpfile64.