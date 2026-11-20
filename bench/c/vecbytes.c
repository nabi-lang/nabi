// C counterpart of bench/vecbytes.nabi.
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>

static int64_t count(const uint8_t *bs, size_t n, uint8_t c) {
    int64_t k = 0;
    for (size_t i = 0; i < n; i++) {
        if (bs[i] == c) {
            k += 1;
        }
    }
    return k;
}

static size_t find(const uint8_t *bs, size_t n, uint8_t c, size_t from) {
    for (size_t i = from; i < n; i++) {
        if (bs[i] == c) {
            return i;
        }
    }
    return n;
}

int main(void) {
    size_t n = (size_t)8 << 20;
    uint8_t *bs = malloc(n);
    uint64_t x = 7;
    for (size_t i = 0; i < n; i++) {
        x = x * 6364136223846793005ull + 1442695040888963407ull;
        uint64_t r = (x >> 33) % 1000;
        bs[i] = r < 150 ? 32 : r < 151 ? 10 : (uint8_t)(97 + r % 26);
    }
    int64_t spaces = 0, lines = 0;
    size_t longest = 0;
    for (int k = 0; k < 30; k++) {
        spaces += count(bs, n, 32);
        size_t at = 0;
        while (at < n) {
            size_t e = find(bs, n, 10, at);
            if (e - at > longest) {
                longest = e - at;
            }
            lines += 1;
            at = e + 1;
        }
    }
    printf("%lld %lld %zu\n", (long long)spaces, (long long)lines, longest);
    return 0;
}
