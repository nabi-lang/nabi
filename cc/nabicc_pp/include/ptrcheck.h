/* ptrcheck.h: nabic's; bounds-safety attributes are off. */
#ifndef __PTRCHECK_H
#define __PTRCHECK_H
#define __has_ptrcheck 0
#define __has_bounds_safety_attributes 0
#define __single
#define __unsafe_indexable
#define __counted_by(N)
#define __counted_by_or_null(N)
#define __sized_by(N)
#define __sized_by_or_null(N)
#define __ended_by(E)
#define __terminated_by(T)
#define __null_terminated
#define __bidi_indexable
#define __indexable
#define __unsafe_late_const
#define __array_decay_discards_count_in_parameters
#define __ptrcheck_abi_assume_single()
#define __ptrcheck_abi_assume_indexable()
#define __ptrcheck_abi_assume_bidi_indexable()
#define __ptrcheck_abi_assume_unsafe_indexable()
#define __ptrcheck_unavailable
#define __ptrcheck_unavailable_r(REPLACEMENT)
#define __unsafe_forge_bidi_indexable(T, P, S) ((T)(P))
#define __unsafe_forge_single(T, P) ((T)(P))
#define __unsafe_forge_terminated_by(T, P, E) ((T)(P))
#define __unsafe_forge_null_terminated(T, P) ((T)(P))
#define __terminated_by_to_indexable(P) (P)
#define __null_terminated_to_indexable(P) (P)
#define __unsafe_terminated_by_to_indexable(P) (P)
#define __unsafe_null_terminated_to_indexable(P) (P)
#define __IGNORE_REST(P, ...) (P)
#define __unsafe_terminated_by_from_indexable(T, ...) __IGNORE_REST(__VA_ARGS__)
#define __unsafe_null_terminated_from_indexable(...) __IGNORE_REST(__VA_ARGS__)
#define __ptr_lower_bound(X) (X)
#define __ptr_upper_bound(X) (X)
#endif
