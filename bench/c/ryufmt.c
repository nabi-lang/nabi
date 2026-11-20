// C counterpart of bench/ryufmt.nabi: the same Ryu d2d core, with the
// same 64-bit-halves multiply (no 128-bit type) and digit output.
#include <stdint.h>
#include <stdio.h>
#include <string.h>

#define POW5_BITCOUNT 125

static uint64_t pow5[54][2];

static uint32_t pow5bits(int32_t e) { return (uint32_t)(((e * 1217359) >> 19) + 1); }
static uint32_t log10pow5(int32_t e) { return (uint32_t)((e * 732923) >> 20); }

static void full_mul(uint64_t a, uint64_t b, uint64_t *hi, uint64_t *lo) {
    uint64_t a0 = a & 0xffffffff, a1 = a >> 32, b0 = b & 0xffffffff, b1 = b >> 32;
    uint64_t p00 = a0 * b0, p01 = a0 * b1, p10 = a1 * b0, p11 = a1 * b1;
    uint64_t mid = (p00 >> 32) + (p01 & 0xffffffff) + (p10 & 0xffffffff);
    *lo = (mid << 32) | (p00 & 0xffffffff);
    *hi = p11 + (p01 >> 32) + (p10 >> 32) + (mid >> 32);
}

static uint64_t mul_shift(uint64_t m, const uint64_t *mul, uint32_t j) {
    uint64_t h0, l0, h1, l1;
    full_mul(m, mul[0], &h0, &l0);
    full_mul(m, mul[1], &h1, &l1);
    uint64_t lo = h0 + l1;
    uint64_t hi = h1 + (lo < h0);
    uint32_t s = j - 64;
    return (hi << (64 - s)) | (lo >> s);
}

static void make_table(void) {
    uint64_t hi = 0, lo = 1;
    for (int i = 0; i < 54; i++) {
        uint32_t bits = hi ? 128 - __builtin_clzll(hi) : 64 - __builtin_clzll(lo);
        uint32_t sh = POW5_BITCOUNT - bits;
        uint64_t h = sh >= 64 ? lo << (sh - 64) : sh == 0 ? hi : (hi << sh) | (lo >> (64 - sh));
        uint64_t l = sh >= 64 ? 0 : lo << sh;
        pow5[i][0] = l;
        pow5[i][1] = h;
        // times 5
        uint64_t l4 = lo << 2, h4 = (hi << 2) | (lo >> 62);
        uint64_t nl = l4 + lo;
        hi = h4 + hi + (nl < l4);
        lo = nl;
    }
}

// Shortest digits of positive x in [2^-60, 2^53): output, exponent.
static uint64_t d2d(double x, int32_t *exp) {
    uint64_t bits;
    memcpy(&bits, &x, 8);
    uint64_t mant = bits & ((1ULL << 52) - 1);
    uint32_t ie = (uint32_t)(bits >> 52) & 0x7ff;
    int32_t e2 = (int32_t)ie - 1023 - 52 - 2;
    uint64_t m2 = (1ULL << 52) | mant;
    int even = (m2 & 1) == 0;
    uint64_t mv = 4 * m2;
    uint32_t mmshift = mant != 0 || ie <= 1;
    int vm_tz = 0, vr_tz = 0;
    uint32_t q = log10pow5(-e2) - (-e2 > 1);
    int32_t e10 = (int32_t)q + e2;
    int32_t i = -e2 - (int32_t)q;
    int32_t k = (int32_t)pow5bits(i) - POW5_BITCOUNT;
    uint32_t j = (uint32_t)((int32_t)q - k);
    uint64_t vr = mul_shift(4 * m2, pow5[i], j);
    uint64_t vp = mul_shift(4 * m2 + 2, pow5[i], j);
    uint64_t vm = mul_shift(4 * m2 - 1 - mmshift, pow5[i], j);
    if (q <= 1) {
        vr_tz = 1;
        if (even)
            vm_tz = mmshift == 1;
        else
            vp--;
    } else if (q < 63) {
        vr_tz = (mv & ((1ULL << q) - 1)) == 0;
    }
    int32_t removed = 0;
    uint64_t out;
    if (vm_tz || vr_tz) {
        uint8_t last = 0;
        for (;;) {
            uint64_t vpd = vp / 10, vmd = vm / 10;
            if (vpd <= vmd)
                break;
            uint32_t vmm = (uint32_t)(vm - 10 * vmd);
            uint64_t vrd = vr / 10;
            uint32_t vrm = (uint32_t)(vr - 10 * vrd);
            vm_tz &= vmm == 0;
            vr_tz &= last == 0;
            last = (uint8_t)vrm;
            vr = vrd;
            vp = vpd;
            vm = vmd;
            removed++;
        }
        if (vm_tz) {
            for (;;) {
                uint64_t vmd = vm / 10;
                uint32_t vmm = (uint32_t)(vm - 10 * vmd);
                if (vmm != 0)
                    break;
                uint64_t vpd = vp / 10, vrd = vr / 10;
                uint32_t vrm = (uint32_t)(vr - 10 * vrd);
                vr_tz &= last == 0;
                last = (uint8_t)vrm;
                vr = vrd;
                vp = vpd;
                vm = vmd;
                removed++;
            }
        }
        if (vr_tz && last == 5 && vr % 2 == 0)
            last = 4;
        out = vr + ((vr == vm && (!even || !vm_tz)) || last >= 5);
    } else {
        int up = 0;
        uint64_t vpd = vp / 100, vmd = vm / 100;
        if (vpd > vmd) {
            uint64_t vrd = vr / 100;
            uint32_t vrm = (uint32_t)(vr - 100 * vrd);
            up = vrm >= 50;
            vr = vrd;
            vp = vpd;
            vm = vmd;
            removed += 2;
        }
        for (;;) {
            uint64_t vpd = vp / 10, vmd = vm / 10;
            if (vpd <= vmd)
                break;
            uint64_t vrd = vr / 10;
            uint32_t vrm = (uint32_t)(vr - 10 * vrd);
            up = vrm >= 5;
            vr = vrd;
            vp = vpd;
            vm = vmd;
            removed++;
        }
        out = vr + (vr == vm || up);
    }
    *exp = e10 + removed;
    return out;
}

static char text[8192];
static size_t tlen;

// "<digits>e<exp>" of x.
static void format(double x) {
    int32_t e;
    uint64_t d = d2d(x, &e);
    char tmp[20];
    size_t n = 0;
    while (d >= 10) {
        tmp[n++] = (char)(d % 10 + 48);
        d /= 10;
    }
    tmp[n++] = (char)(d + 48);
    for (size_t k = 0; k < n; k++)
        text[tlen++] = tmp[n - 1 - k];
    text[tlen++] = 'e';
    if (e < 0) {
        text[tlen++] = '-';
        e = -e;
    }
    if (e >= 10)
        text[tlen++] = (char)(e / 10 + 48);
    text[tlen++] = (char)(e % 10 + 48);
    text[tlen++] = ' ';
}

int main(void) {
    make_table();
    uint64_t x = 3, sum = 0, bad = 0;
    for (int r = 0; r < 2000000; r++) {
        x = x * 6364136223846793005ULL + 1442695040888963407ULL;
        // [1e-3, ~1e15): 1.xxx times 2^(-10..49)
        uint64_t b = ((uint64_t)(1013 + (x >> 58) % 60) << 52) | (x >> 12);
        double v;
        memcpy(&v, &b, 8);
        format(v);
        if (tlen > 4000) {
            sum = sum * 31 + tlen + (unsigned char)text[r % 1000];
            tlen = 0;
        }
#ifdef CHECK
        {
            int32_t e;
            uint64_t d = d2d(v, &e);
            char g[40];
            snprintf(g, sizeof g, "%llue%d", (unsigned long long)d, e);
            double back;
            sscanf(g, "%lf", &back);
            bad += back != v;
        }
#endif
    }
    printf("%llu %zu %.20s\n", (unsigned long long)sum, tlen, text);
#ifdef CHECK
    printf("bad %llu\n", (unsigned long long)bad);
#endif
    return 0;
}
