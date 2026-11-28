#include <stdlib.h>

#include "ring.h"

struct ring {
	size_t cap;
	size_t head; /* index of the oldest value */
	size_t len;
	uint32_t buf[]; /* cap values */
};

int ring_new(size_t cap, struct ring **out)
{
	struct ring *r;

	if (cap == 0 || cap > (SIZE_MAX - sizeof(*r)) / sizeof(uint32_t))
		return RING_EINVAL;
	r = malloc(sizeof(*r) + cap * sizeof(uint32_t));
	if (!r)
		return RING_ENOMEM;
	r->cap = cap;
	r->head = 0;
	r->len = 0;
	*out = r;
	return RING_OK;
}

void ring_free(struct ring *r)
{
	free(r);
}

int ring_push(struct ring *r, uint32_t value)
{
	if (r->len == r->cap)
		return RING_EFULL;
	r->buf[(r->head + r->len) % r->cap] = value;
	r->len++;
	return RING_OK;
}

int ring_pop(struct ring *r, uint32_t *out)
{
	if (r->len == 0)
		return RING_EEMPTY;
	*out = r->buf[r->head];
	r->head = (r->head + 1) % r->cap;
	r->len--;
	return RING_OK;
}

size_t ring_len(const struct ring *r)
{
	return r->len;
}

/* Oldest first; fn must not touch r. */
void ring_for_each(const struct ring *r, ring_visit_fn fn, void *ctx)
{
	for (size_t i = 0; i < r->len; i++)
		fn(r->buf[(r->head + i) % r->cap], ctx);
}

/* FNV-1a over each value's four bytes, little-endian, oldest first. */
uint32_t ring_fnv1a(const struct ring *r)
{
	uint32_t h = 2166136261u;

	for (size_t i = 0; i < r->len; i++) {
		uint32_t v = r->buf[(r->head + i) % r->cap];

		for (int b = 0; b < 4; b++) {
			h ^= (v >> (8 * b)) & 0xffu;
			h *= 16777619u;
		}
	}
	return h;
}
