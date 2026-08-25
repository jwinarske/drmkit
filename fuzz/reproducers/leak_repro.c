// Minimal reproducer: libdisplay-info leaks 4 bytes parsing an EDID whose
// CTA-861 extension carries an InfoFrame Data Block.
//
//   cc -g -fsanitize=address -o leak_repro leak_repro.c $(pkg-config --cflags --libs libdisplay-info)
//   ./leak_repro edid-libdisplay-info-leak-minimal.bin
//
// Exits 0 either way; ASan/LSan reports the leak at exit.
#include <libdisplay-info/info.h>
#include <stdio.h>
#include <stdlib.h>

int main(int argc, char **argv) {
    if (argc < 2) { fprintf(stderr, "usage: %s EDID\n", argv[0]); return 2; }
    FILE *f = fopen(argv[1], "rb");
    if (!f) { perror("fopen"); return 2; }
    static unsigned char buf[32768];
    size_t n = fread(buf, 1, sizeof buf, f);
    fclose(f);
    printf("parsing %zu bytes\n", n);

    struct di_info *info = di_info_parse_edid(buf, n);
    if (!info) { printf("di_info_parse_edid returned NULL\n"); return 0; }
    // Everything the caller is given back is freed here.
    di_info_destroy(info);
    printf("parsed and destroyed cleanly\n");
    return 0;
}
