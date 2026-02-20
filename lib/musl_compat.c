// Compatibility shims for musl libc (which doesn't have *64 functions)
// Provide strong definitions of *64 functions that map to standard ones.

#define _GNU_SOURCE
#include <stdio.h>
#include <errno.h>

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
int open64(const char *pathname, int flags, ...) {
    // This is more complex; we'll not implement unless needed.
    // Just return -1 with errno ENOSYS.
    errno = ENOSYS;
    return -1;
}

// Note: We don't need to implement all *64 functions, only those referenced.
// Lua references fopen64, freopen64, tmpfile64.