/* Fixed-capacity FIFO ring of 32-bit values, plus an FNV-1a checksum. */
#ifndef RING_H
#define RING_H

#include <stddef.h>
#include <stdint.h>

#define RING_OK 0
#define RING_EINVAL -1
#define RING_ENOMEM -2
#define RING_EFULL -3
#define RING_EEMPTY -4

struct ring;

typedef void (*ring_visit_fn)(uint32_t value, void *ctx);

int ring_new(size_t cap, struct ring **out);
void ring_free(struct ring *r);
int ring_push(struct ring *r, uint32_t value);
int ring_pop(struct ring *r, uint32_t *out);
size_t ring_len(const struct ring *r);
void ring_for_each(const struct ring *r, ring_visit_fn fn, void *ctx);
uint32_t ring_fnv1a(const struct ring *r);

#endif
