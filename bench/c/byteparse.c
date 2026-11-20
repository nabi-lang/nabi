// C counterpart of bench/byteparse.nabi.
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static const double POW10[23] = {1e0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8, 1e9, 1e10, 1e11,
                                 1e12, 1e13, 1e14, 1e15, 1e16, 1e17, 1e18, 1e19, 1e20, 1e21, 1e22};

typedef struct {
    int ok, is_float;
    int64_t i;
    double f;
    size_t end;
} Num;

static Num fail(size_t at) { return (Num){0, 0, 0, 0.0, at}; }

static Num number(const unsigned char *b, size_t n, size_t at) {
    if (at >= n)
        return fail(at);
    int neg = b[at] == '-';
    size_t int_start = neg ? at + 1 : at, j = int_start;
    uint64_t m = 0;
    while (j < n && b[j] >= '0' && b[j] <= '9')
        m = m * 10 + (b[j++] - '0');
    if (j == int_start)
        return fail(at);
    if (b[int_start] == '0' && j - int_start > 1)
        return fail(at);
    size_t nd = j - int_start;
    int is_float = 0;
    int64_t frac = 0;
    if (j < n && b[j] == '.') {
        is_float = 1;
        size_t k = j + 1;
        while (k < n && b[k] >= '0' && b[k] <= '9')
            m = m * 10 + (b[k++] - '0');
        if (k == j + 1)
            return fail(at);
        frac = (int64_t)(k - j - 1);
        nd += k - j - 1;
        j = k;
    }
    int64_t exp = 0;
    if (j < n && (b[j] == 'e' || b[j] == 'E')) {
        is_float = 1;
        j++;
        int eneg = 0;
        if (j < n && (b[j] == '+' || b[j] == '-')) {
            eneg = b[j] == '-';
            j++;
        }
        size_t k = j;
        uint64_t e = 0;
        while (k < n && b[k] >= '0' && b[k] <= '9')
            e = e * 10 + (b[k++] - '0');
        if (k == j)
            return fail(at);
        exp = k - j > 18 ? 100000 : (int64_t)(e < 100000 ? e : 100000);
        if (eneg)
            exp = -exp;
        j = k;
    }
    if (!is_float) {
        if (j - int_start > 18)
            return fail(at);
        int64_t v = (int64_t)m;
        return (Num){1, 0, neg ? -v : v, 0.0, j};
    }
    if (nd > 19)
        return fail(at);
    int64_t e10 = exp - frac;
    if (m == 0)
        return (Num){1, 1, 0, neg ? -0.0 : 0.0, j};
    if (m <= 9007199254740992ULL && e10 >= -22 && e10 <= 22) {
        double x = (double)m;
        x = e10 >= 0 ? x * POW10[e10] : x / POW10[-e10];
        return (Num){1, 1, 0, neg ? -x : x, j};
    }
    return fail(at);
}

int main(void) {
    size_t cap = 1 << 24, len = 0;
    char *s = malloc(cap);
    uint64_t x = 11;
    for (int i = 0; i < 400000; i++) {
        x = x * 6364136223846793005ULL + 1442695040888963407ULL;
        unsigned long long r = x >> 20;
        switch (r % 4) {
        case 0: len += sprintf(s + len, "%llu", r % 1000000000000ULL); break;
        case 1: len += sprintf(s + len, "-%llu.%llu", r % 100000, r % 1000); break;
        case 2: len += sprintf(s + len, "%llu.%llue-%llu", r % 1000, r % 10000000, r % 12); break;
        default: len += sprintf(s + len, "%llu.%lluE+%llu", r % 100, r % 100, r % 9); break;
        }
        s[len++] = ',';
    }
    const unsigned char *b = (const unsigned char *)s;
    int64_t ints = 0, count = 0;
    double floats = 0.0;
    for (int r = 0; r < 10; r++) {
        size_t i = 0;
        while (i < len) {
            Num v = number(b, len, i);
            if (!v.ok)
                return 1;
            if (v.is_float)
                floats += v.f;
            else
                ints = (int64_t)((uint64_t)ints + (uint64_t)v.i);
            count++;
            i = v.end + 1;
        }
    }
    uint64_t bits;
    memcpy(&bits, &floats, 8);
    printf("%lld %lld %llu\n", (long long)count, (long long)ints, (unsigned long long)bits);
    return 0;
}
