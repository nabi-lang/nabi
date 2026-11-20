// C counterpart of bench/vecsum.nabi.
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>

static int32_t sum(const int32_t *xs, size_t n) {
    int32_t s = 0;
    for (size_t i = 0; i < n; i++) {
        s += xs[i];
    }
    return s;
}

static uint8_t wsum(const uint8_t *bs, size_t n) {
    uint8_t s = 0;
    for (size_t i = 0; i < n; i++) {
        s += bs[i];
    }
    return s;
}

int main(void) {
    size_t n = (size_t)1 << 20;
    int32_t *xs = malloc(n * sizeof *xs);
    uint8_t *bs = malloc(n);
    uint64_t x = 1;
    for (size_t i = 0; i < n; i++) {
        x = x * 6364136223846793005ull + 1442695040888963407ull;
        xs[i] = (int32_t)((x >> 33) % 201) - 100;
        bs[i] = (uint8_t)(x >> 40);
    }
    int64_t t = 0;
    uint64_t w = 0;
    for (int r = 0; r < 400; r++) {
        xs[r] += 1;
        t += sum(xs, n);
        w += wsum(bs, n);
    }
    printf("%lld %llu\n", (long long)t, (unsigned long long)w);
    return 0;
}
