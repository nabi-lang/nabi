// C counterpart of bench/vecdot.nabi.
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>

static double dot(const double *xs, const double *ys, size_t n) {
    double s = 0.0;
    for (size_t i = 0; i < n; i++) {
        s += xs[i] * ys[i];
    }
    return s;
}

static int32_t idot(const int32_t *xs, const int32_t *ys, size_t n) {
    uint32_t s = 0;
    for (size_t i = 0; i < n; i++) {
        s += (uint32_t)xs[i] * (uint32_t)ys[i];
    }
    return (int32_t)s;
}

int main(void) {
    size_t n = (size_t)1 << 20;
    double *xs = malloc(n * sizeof *xs), *ys = malloc(n * sizeof *ys);
    int32_t *is = malloc(n * sizeof *is), *js = malloc(n * sizeof *js);
    for (size_t i = 0; i < n; i++) {
        xs[i] = (double)(i % 100) * 0.01;
        ys[i] = (double)(i % 37) * 0.5;
        is[i] = (int32_t)(i % 1001) - 500;
        js[i] = (int32_t)(i % 77);
    }
    double s = 0.0;
    int64_t t = 0;
    for (int r = 0; r < 300; r++) {
        is[r] += 1;
        s += dot(xs, ys, n);
        t += idot(is, js, n);
    }
    printf("%.3f %lld\n", s, (long long)t);
    return 0;
}
