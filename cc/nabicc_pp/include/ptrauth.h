/* ptrauth.h: nabic's; pointer authentication is off. */
#ifndef __PTRAUTH_H
#define __PTRAUTH_H

typedef enum {
  ptrauth_key_asia = 0,
  ptrauth_key_asib = 1,
  ptrauth_key_asda = 2,
  ptrauth_key_asdb = 3,
  ptrauth_key_process_independent_code = ptrauth_key_asia,
  ptrauth_key_process_dependent_code = ptrauth_key_asib,
  ptrauth_key_process_independent_data = ptrauth_key_asda,
  ptrauth_key_process_dependent_data = ptrauth_key_asdb,
  ptrauth_key_function_pointer = ptrauth_key_process_independent_code,
  ptrauth_key_return_address = ptrauth_key_process_dependent_code,
  ptrauth_key_frame_pointer = ptrauth_key_process_dependent_data,
  ptrauth_key_block_function = ptrauth_key_asia,
  ptrauth_key_cxx_vtable_pointer = ptrauth_key_asda,
  ptrauth_key_method_list_pointer = ptrauth_key_asda,
  ptrauth_key_objc_isa_pointer = ptrauth_key_process_independent_data,
  ptrauth_key_objc_super_pointer = ptrauth_key_process_independent_data,
  ptrauth_key_block_descriptor_pointer = ptrauth_key_process_independent_data,
  ptrauth_key_objc_sel_pointer = ptrauth_key_process_dependent_data,
  ptrauth_key_objc_class_ro_pointer = ptrauth_key_process_dependent_data
} ptrauth_key;

typedef __UINTPTR_TYPE__ ptrauth_extra_data_t;
typedef __UINTPTR_TYPE__ ptrauth_generic_signature_t;

#define ptrauth_strip(__value, __key) ((void)__key, __value)
#define ptrauth_blend_discriminator(__pointer, __integer)                      \
  ((void)__pointer, (void)__integer, ((ptrauth_extra_data_t)0))
#define ptrauth_type_discriminator(__type) ((ptrauth_extra_data_t)0)
#define ptrauth_function_pointer_type_discriminator(__type)                    \
  ((ptrauth_extra_data_t)0)
#define ptrauth_string_discriminator(__string) ((ptrauth_extra_data_t)0)
#define ptrauth_sign_constant(__value, __key, __data)                          \
  ((void)__key, (void)__data, __value)
#define ptrauth_sign_unauthenticated(__value, __key, __data)                   \
  ((void)__key, (void)__data, __value)
#define ptrauth_auth_and_resign(__value, __old_key, __old_data, __new_key,     \
                                __new_data)                                    \
  ((void)__old_key, (void)__old_data, (void)__new_key, (void)__new_data,       \
   __value)
#define ptrauth_auth_with_pc_and_resign(__value, __old_key, __old_data,        \
                                        __old_pc, __new_key, __new_data)       \
  __value
#define ptrauth_auth_function(__value, __old_key, __old_data)                  \
  ((void)__old_key, (void)__old_data, __value)
#define ptrauth_auth_data(__value, __old_key, __old_data)                      \
  ((void)__old_key, (void)__old_data, __value)
#define ptrauth_sign_generic_data(__value, __data)                             \
  ((void)__value, (void)__data, ((ptrauth_generic_signature_t)0))
#define ptrauth_nop_cast(__type, __value) ((__type)__value)
#define ptrauth_cxx_vtable_pointer(key, address_discrimination,                \
                                   extra_discrimination...)
#define __ptrauth_function_pointer(__typekey)
#define __ptrauth_return_address
#define __ptrauth_block_invocation_pointer
#define __ptrauth_block_copy_helper
#define __ptrauth_block_destroy_helper
#define __ptrauth_block_byref_copy_helper
#define __ptrauth_block_byref_destroy_helper
#define __ptrauth_block_descriptor_pointer
#define __ptrauth_objc_method_list_imp
#define __ptrauth_objc_method_list_pointer
#define __ptrauth_isa_discriminator 0x6AE1
#define __ptrauth_super_discriminator 0xB5AB
#define __ptrauth_objc_isa_pointer
#define __ptrauth_objc_isa_uintptr
#define __ptrauth_objc_super_pointer
#define __ptrauth_objc_sel
#define __ptrauth_objc_class_ro
#define __ptrauth_cxx_vtable_pointer
#define __ptrauth_cxx_vtt_vtable_pointer
#define __ptrauth_cxx_virtual_function_pointer(__declkey)
#define __ptrauth_swift_function_pointer(__typekey)
#define __ptrauth_swift_class_method_pointer(__declkey)
#define __ptrauth_swift_protocol_witness_function_pointer(__declkey)
#define __ptrauth_swift_value_witness_function_pointer(__key)
#define __ptrauth_swift_heap_object_destructor
#define __ptrauth_init_fini_discriminator 0xd9d4

#endif
