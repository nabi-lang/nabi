// C counterpart of bench/wordload.nabi.
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static uint64_t le_word8(const uint8_t *b, size_t at) {
    uint64_t v;
    memcpy(&v, b + at, 8);
    return v;
}

static uint64_t words(const uint8_t *b, size_t n) {
    uint64_t h = 0;
    for (size_t i = 0; i + 8 <= n; i += 8)
        h = (h ^ le_word8(b, i)) * 0x100000001b3ULL;
    return h;
}

static uint64_t rot(uint64_t x, uint64_t n) { return (x << (n % 64)) | (x >> ((64 - n % 64) % 64)); }

static uint64_t shifts(uint64_t x, size_t m) {
    uint64_t h = 0;
    for (size_t k = 0; k < m; k++) {
        x = x * 6364136223846793005ULL + 1442695040888963407ULL;
        h = (h + rot(x, x >> 58)) ^ (x << (k & 63));
    }
    return h;
}

int main(void) {
    size_t n = 1 << 16;
    uint8_t *b = malloc(n);
    uint64_t x = 5;
    for (size_t i = 0; i < n; i++) {
        x = x * 6364136223846793005ULL + 1442695040888963407ULL;
        b[i] = (uint8_t)(x >> 56);
    }
    uint64_t h = 0;
    for (size_t r = 0; r < 400; r++)
        h += words(b, n - (r % 8));
    printf("%llu %llu\n", (unsigned long long)h, (unsigned long long)shifts(h, 20000000));
    return 0;
}
