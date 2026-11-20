// C counterpart of bench/digitspush.nabi.
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>

static char *buf;
static size_t len, cap = 64;

static void push_byte(char b) {
    if (len == cap) {
        cap *= 2;
        buf = realloc(buf, cap);
    }
    buf[len++] = b;
}

static void digits(uint64_t v) {
    char tmp[20];
    size_t n = 0;
    while (v >= 10) {
        tmp[n++] = (char)(v % 10 + 48);
        v /= 10;
    }
    tmp[n++] = (char)(v + 48);
    for (size_t k = 0; k < n; k++)
        push_byte(tmp[n - 1 - k]);
}

int main(void) {
    buf = malloc(cap);
    uint64_t x = 7, sum = 0;
    for (uint64_t r = 0; r < 3000000; r++) {
        x = x * 6364136223846793005ULL + 1442695040888963407ULL;
        uint64_t v = 10000000000000000ULL + (x >> 8) % 90000000000000000ULL;
        digits(v);
        if (len > 4000) {
            sum += len + (unsigned char)buf[r % 17];
            len = 0;
        }
    }
    printf("%llu %zu %.17s\n", (unsigned long long)sum, len, buf);
    return 0;
}
