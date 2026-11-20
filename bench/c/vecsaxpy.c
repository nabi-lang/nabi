// C counterpart of bench/vecsaxpy.nabi.
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>

static void axpy(double a, const double *xs, double *ys, size_t n) {
    for (size_t i = 0; i < n; i++) {
        ys[i] = a * xs[i] + ys[i];
    }
}

static void affine(const int16_t *xs, int16_t *ys, size_t n) {
    for (size_t i = 0; i < n; i++) {
        ys[i] = (int16_t)(uint16_t)((uint16_t)xs[i] * 3u + 1u);
    }
}

int main(void) {
    size_t n = (size_t)1 << 20;
    double *xs = malloc(n * sizeof *xs), *ys = malloc(n * sizeof *ys);
    int16_t *hs = malloc(n * sizeof *hs), *gs = calloc(n, sizeof *gs);
    for (size_t i = 0; i < n; i++) {
        xs[i] = (double)(i % 1000) * 0.001;
        ys[i] = (double)(i % 7);
        hs[i] = (int16_t)(i % 30011);
    }
    for (int r = 0; r < 500; r++) {
        axpy(0.5 + (double)(r % 3) * 0.25, xs, ys, n);
        affine(hs, gs, n);
        hs[r] = gs[r + 1];
    }
    double s = 0.0;
    int64_t k = 0;
    for (size_t i = 0; i < n; i++) {
        s += ys[i];
        k += gs[i];
    }
    printf("%.6f %lld\n", s, (long long)k);
    return 0;
}
